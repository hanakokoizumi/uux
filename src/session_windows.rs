use super::*;

#[derive(Default)]
pub(in crate::app) struct SessionWindows {
    pub remote: bool,
    pub files: bool,
    pub terminal: bool,
    pub ports: bool,
    pub clipboard_active: bool,
}
impl SessionWindows {
    fn in_use(&self) -> bool {
        self.remote || self.files || self.terminal || self.ports
    }
}

#[derive(Default)]
pub(in crate::app) struct WindowProbe {
    started: Option<Instant>,
    stage: u8,
    directory_after_close: bool,
    directory_requested: bool,
    remote_native: bool,
    files_native: bool,
    observations: Vec<Value>,
    completed: bool,
}

impl Desktop {
    pub(in crate::app) fn show_remote_window(&mut self) {
        self.ui_state.windows.remote = true;
        self.raise_window(self.viewport("remote"));
    }

    pub(super) fn raise_window(&self, target: egui::ViewportId) {
        for command in [
            egui::ViewportCommand::Visible(true),
            egui::ViewportCommand::Minimized(false),
            egui::ViewportCommand::Focus,
        ] {
            self.context.send_viewport_cmd_to(target, command);
        }
        self.context.request_repaint_of(target);
    }

    pub(in crate::app) fn window_probe_timeout(&mut self) {
        let Some(path) = &self.options.window_test else {
            return;
        };
        if self.window_probe.completed {
            return;
        }
        self.window_probe.completed = true;
        let report = json!({
            "passed":false,
            "reason":"timeout",
            "phase":self.phase,
            "connected":self.connected,
            "observations":self.window_probe.observations
        });
        let _ = std::fs::write(path, serde_json::to_vec_pretty(&report).unwrap());
        eprintln!("窗口生命周期验证：超时，已保存失败报告");
    }

    pub(super) fn close_remote_window(&mut self) {
        self.release();
        self.control(json!({"op":"clipboard_focus","enabled":false}));
        self.ui_state.windows.clipboard_active = false;
        self.ui_state.windows.remote = false;
        self.ui_state.text_window = false;
        self.ui_state.remote_action = None;
        self.ui_state.super_confirm = None;
        self.fullscreen = false;
    }

    pub(super) fn session_windows(&mut self, ctx: &egui::Context) {
        if self.ui_state.windows.remote
            && self.ui_state.screens.iter().all(|screen| {
                self.ui_state
                    .hidden_screens
                    .contains(&(screen["id"].as_i64().unwrap_or(0) as i32))
            })
        {
            self.ui_state.hidden_screens.remove(&self.ui_state.screen);
        }
        if self.connected && self.ui_state.windows.remote != self.video_wait_requested {
            self.control(json!({"op":"expect_video","enabled":self.ui_state.windows.remote}));
            self.video_wait_requested = self.ui_state.windows.remote;
        }
        if self.ui_state.windows.remote {
            let _render = crate::perf::UiScope::new("remote_render");
            let title = self.active_device.as_ref().map_or("远程桌面", |d| &d.name);
            ctx.show_viewport_immediate(
                self.viewport("remote"),
                egui::ViewportBuilder::default()
                    .with_icon(crate::brand::icon())
                    .with_title(format!("{title} — UUX"))
                    .with_inner_size([1280.0, 760.0])
                    .with_min_inner_size([640.0, 400.0])
                    .with_resizable(true)
                    .with_decorations(false)
                    .with_active(
                        self.options.screenshot.is_none() || self.options.diagnostic_stdin,
                    ),
                |ui, class| {
                    let _ui = crate::perf::UiScope::new("remote_ui");
                    crate::perf::trace_focus(ui.ctx(), "remote");
                    self.window_probe.remote_native = class == egui::ViewportClass::Immediate;
                    if ui.input(|i| i.viewport().close_requested()) {
                        self.close_remote_window();
                        return;
                    }
                    self.ui_state.resize_captured = self.ui_state.remote_resize.intercept(
                        ui.ctx(),
                        self.error.is_none()
                            && !self.ui_state.text_window
                            && self.ui_state.remote_action.is_none()
                            && self.ui_state.super_confirm.is_none()
                            && !egui::Popup::is_any_open(ui.ctx()),
                    );
                    if self.ui_state.resize_captured {
                        self.pending_input.clear();
                        let releases = self.remote_input.suspend();
                        self.inputs(releases);
                    } else {
                        self.remote_events(ui.ctx());
                    }
                    self.remote_toolbar(ui);
                    egui::CentralPanel::default()
                        .frame(egui::Frame::new().fill(Color32::from_rgb(25, 29, 34)))
                        .show(ui, |ui| self.canvas(ui));
                    self.remote_dialogs(ui.ctx());
                    self.ui_state.remote_resize.paint_cursor(ui.ctx());
                    self.capture_window(ui.ctx());
                },
            );
        }
        if self.ui_state.windows.files {
            let _render = crate::perf::UiScope::new("files_render");
            ctx.show_viewport_immediate(
                self.viewport("files"),
                egui::ViewportBuilder::default()
                    .with_icon(crate::brand::icon())
                    .with_title("文件传输 — UUX")
                    .with_inner_size([980.0, 620.0])
                    .with_min_inner_size([880.0, 560.0])
                    .with_resizable(true)
                    .with_decorations(false)
                    .with_active(
                        self.options.screenshot.is_none() || self.options.diagnostic_stdin,
                    ),
                |ui, class| {
                    let _ui = crate::perf::UiScope::new("files_ui");
                    self.window_probe.files_native = class == egui::ViewportClass::Immediate;
                    if ui.input(|i| i.viewport().close_requested()) {
                        self.ui_state.windows.files = false;
                        self.ui_state.file_dialog = None;
                        return;
                    }
                    ui.input_mut(|i| Self::local_clipboard_events(&mut i.events));
                    self.ui_state.file_resize.intercept(
                        ui.ctx(),
                        self.ui_state.file_dialog.is_none() && !egui::Popup::is_any_open(ui.ctx()),
                    );
                    self.files_view(ui);
                    self.ui_state.file_resize.paint_cursor(ui.ctx());
                    self.capture_window(ui.ctx());
                },
            );
        }
        self.terminal_window(ctx);
        self.ports_window(ctx);
        if !self.ui_state.windows.in_use()
            && !self
                .ui_state
                .device_tools
                .pending
                .as_ref()
                .is_some_and(|p| p.temporary_session)
            && (self.connected || self.connecting)
            && !self.disconnecting
        {
            self.disconnect();
        }
        self.window_lifecycle_probe(ctx);
    }

    pub(in crate::app) fn local_clipboard_events(events: &mut Vec<egui::Event>) {
        if events.iter().any(|e| {
            matches!(
                e,
                egui::Event::Copy | egui::Event::Cut | egui::Event::Paste(_)
            )
        }) {
            return;
        }
        let keys = events
            .iter()
            .filter_map(|e| match e {
                egui::Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } if modifiers.ctrl && !modifiers.alt => Some(*key),
                _ => None,
            })
            .collect::<Vec<_>>();
        for key in keys {
            match key {
                egui::Key::V => {
                    if let Ok(mut clipboard) = arboard::Clipboard::new()
                        && let Ok(text) = clipboard.get_text()
                        && text.len() <= 1024 * 1024
                    {
                        events.push(egui::Event::Paste(text));
                    }
                }
                egui::Key::C => events.push(egui::Event::Copy),
                egui::Key::X => events.push(egui::Event::Cut),
                _ => {}
            }
        }
    }

    fn remote_events(&mut self, ctx: &egui::Context) {
        let clipboard_active =
            self.connected && ctx.input(|i| i.focused) && self.ui_state.prefs.clipboard;
        if clipboard_active != self.ui_state.windows.clipboard_active {
            self.ui_state.windows.clipboard_active = clipboard_active;
            self.control(json!({"op":"clipboard_focus","enabled":clipboard_active}));
        }
        if !ctx.input(|i| i.focused) {
            self.pending_input.clear();
            let values = self.remote_input.suspend();
            self.inputs(values);
            return;
        }
        let mut events = ctx.input(|i| i.events.clone());
        self.remote_input.release_shortcut = Some(self.ui_state.prefs.release_shortcut.clone());
        if events.iter().any(|event| {
            self.ui_state
                .prefs
                .local_shortcuts
                .iter()
                .any(|s| crate::input::shortcut_matches(s, event))
        }) {
            self.release();
            events.retain(|e| {
                !matches!(
                    e,
                    egui::Event::Key { .. }
                        | egui::Event::Text(_)
                        | egui::Event::ModifiersChanged(_)
                )
            });
        }
        if events
            .iter()
            .any(|e| crate::input::shortcut_matches(&self.ui_state.prefs.fullscreen_shortcut, e))
        {
            self.release();
            self.fullscreen = !self.fullscreen;
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(self.fullscreen));
            events.retain(|e| {
                !matches!(
                    e,
                    egui::Event::Key { .. }
                        | egui::Event::Text(_)
                        | egui::Event::ModifiersChanged(_)
                )
            });
        }
        self.pending_input = events.clone();
        if self.remote_input.armed {
            events.retain(|event| {
                !matches!(
                    event,
                    egui::Event::Key { .. }
                        | egui::Event::Text(_)
                        | egui::Event::Ime(_)
                        | egui::Event::Copy
                        | egui::Event::Cut
                        | egui::Event::Paste(_)
                        | egui::Event::PasteImage(_)
                )
            });
        } else {
            Self::local_clipboard_events(&mut events);
        }
        ctx.input_mut(|input| input.events = events);
    }

    pub(in crate::app) fn main_window_close(&mut self, ctx: &egui::Context) {
        // eframe runs the parent UI when a child is visible. Forcing the hidden
        // root surface to present only adds GPU work and can stall all windows.
        if ctx.input(|i| i.viewport().close_requested())
            && self.ui_state.prefs.close_minimizes
            && !self.window_probe.completed
            && !self
                .options
                .quit_after
                .is_some_and(|t| self.started.elapsed() >= t)
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
        }
    }

    pub(super) fn probe_directory_received(&mut self) {
        if self.window_probe.stage == 1 {
            self.window_probe.directory_after_close = true;
        }
    }
    pub(in crate::app) fn window_lifecycle_probe(&mut self, ctx: &egui::Context) {
        let Some(path) = self.options.window_test.clone() else {
            return;
        };
        if self.window_probe.completed {
            return;
        }
        if self.window_probe.started.is_none()
            && self.connected
            && self.ui_state.windows.remote
            && self.ui_state.windows.files
        {
            self.window_probe.started = Some(Instant::now());
            self.window_probe.observations.push(json!({"step":"two_native_windows","pass":self.window_probe.remote_native && self.window_probe.files_native}));
        }
        let Some(started) = self.window_probe.started else {
            return;
        };
        let elapsed = started.elapsed().as_secs();
        match self.window_probe.stage {
            0 if elapsed >= 2 => {
                ctx.send_viewport_cmd_to(self.viewport("remote"), egui::ViewportCommand::Close);
                self.window_probe.stage = 1;
            }
            1 if !self.ui_state.windows.remote
                && elapsed >= 3
                && !self.window_probe.directory_requested =>
            {
                self.window_probe.directory_requested = true;
                self.control(json!({"op":"read_dir","path":self.ui_state.remote_path}));
            }
            1 if self.window_probe.directory_after_close && elapsed >= 5 => {
                self.window_probe.observations.push(json!({"step":"remote_closed_file_channel_operational",
                    "pass":self.connected && self.ui_state.windows.files && !self.ui_state.windows.remote}));
                self.ui_state.windows.remote = true;
                ctx.send_viewport_cmd_to(self.viewport("files"), egui::ViewportCommand::Close);
                self.window_probe.stage = 2;
            }
            2 if elapsed >= 8 && !self.ui_state.windows.files => {
                self.window_probe.observations.push(json!({"step":"files_closed_remote_still_connected",
                    "pass":self.connected && self.ui_state.windows.remote && self.texture.is_some()}));
                ctx.send_viewport_cmd_to(self.viewport("remote"), egui::ViewportCommand::Close);
                self.window_probe.stage = 3;
            }
            3 if !self.connected && !self.connecting => {
                self.window_probe.observations.push(json!({"step":"both_closed_session_released","pass":self.active_device.is_none() && !self.ui_state.windows.in_use()}));
                self.window_probe.completed = true;
            }
            _ => {}
        }
        if elapsed > 18 {
            self.window_probe.completed = true;
        }
        if self.window_probe.completed {
            let passed = self.window_probe.stage == 3
                && self.window_probe.observations.len() == 4
                && self
                    .window_probe
                    .observations
                    .iter()
                    .all(|v| v["pass"] == true);
            let report = json!({"passed":passed,"observations":self.window_probe.observations});
            let _ = std::fs::write(path, serde_json::to_vec_pretty(&report).unwrap());
            eprintln!("窗口生命周期验证：{}", if passed { "通过" } else { "失败" });
            ctx.send_viewport_cmd_to(egui::ViewportId::ROOT, egui::ViewportCommand::Close);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::SessionWindows;
    #[test]
    fn either_window_keeps_the_session_and_reopening_preserves_it() {
        let mut windows = SessionWindows {
            remote: true,
            files: true,
            ..Default::default()
        };
        windows.remote = false;
        assert!(windows.in_use());
        windows.remote = true;
        windows.files = false;
        assert!(windows.in_use());
        windows.remote = false;
        assert!(!windows.in_use());
        windows.files = true;
        assert!(windows.in_use());
        windows.files = false;
        windows.terminal = true;
        assert!(windows.in_use());
        windows.terminal = false;
        windows.ports = true;
        assert!(windows.in_use());
    }
}

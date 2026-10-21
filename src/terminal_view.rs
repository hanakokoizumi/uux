use super::*;

pub(super) struct TerminalView {
    parser: vt100::Parser,
    sent: bool,
    ready: bool,
    status: String,
    shell: String,
    size: [u16; 2],
    selection: Option<((u16, u16), (u16, u16))>,
    composing: bool,
}
impl Default for TerminalView {
    fn default() -> Self {
        Self {
            parser: vt100::Parser::new(30, 100, 2000),
            sent: false,
            ready: false,
            status: "正在连接…".into(),
            shell: "powershell".into(),
            size: [100, 30],
            selection: None,
            composing: false,
        }
    }
}
pub(super) fn viewport_id() -> egui::ViewportId {
    crate::backend::terminal_viewport_id()
}

impl Desktop {
    pub(super) fn open_terminal_for(&mut self, device: Device) {
        if !self.connected && !self.connecting {
            self.connect_for_tools(device);
        }
        self.ui_state.windows.terminal = true;
        if self.connected {
            self.start_terminal();
        }
    }
    pub(in crate::app) fn start_terminal(&mut self) {
        if !self.ui_state.windows.terminal || self.ui_state.terminal.sent {
            return;
        }
        self.ui_state.terminal.sent = true;
        let state = &self.ui_state.terminal;
        self.control(json!({"op":"terminal","action":"open","shell":state.shell,"cols":state.size[0],"rows":state.size[1]}));
    }
    pub(super) fn close_terminal(&mut self) {
        if self.connected && self.ui_state.terminal.sent {
            self.control(json!({"op":"terminal","action":"close"}));
        }
        self.ui_state.windows.terminal = false;
        self.ui_state.terminal = Default::default();
    }
    pub(super) fn terminal_event(&mut self, kind: &str, data: &Value) {
        if !self.ui_state.windows.terminal {
            return;
        }
        let state = &mut self.ui_state.terminal;
        if kind == "terminal_output" {
            if let Ok(bytes) = base64::engine::general_purpose::STANDARD
                .decode(data["data"].as_str().unwrap_or(""))
            {
                state.parser.process(&bytes);
                state.ready = true;
                state.status = "已连接".into();
            }
        } else {
            let result = &data["result"];
            match data["kind"].as_u64() {
                Some(13) if result["code"].as_i64() == Some(0) => {
                    state.ready = true;
                    state.status = "已连接".into();
                }
                Some(6) => {
                    state.ready = false;
                    state.sent = false;
                    state.status = format!("终端已结束（{}）", result["exit_code"]);
                }
                Some(8 | 13 | 15) => {
                    state.ready = false;
                    state.status = format!(
                        "{}（{}）",
                        result["message"].as_str().unwrap_or("远端未能打开终端"),
                        result["code"]
                    );
                }
                _ => {}
            }
        }
    }
    pub(super) fn terminal_window(&mut self, ctx: &egui::Context) {
        if !self.ui_state.windows.terminal {
            return;
        }
        ctx.show_viewport_immediate(
            viewport_id(),
            egui::ViewportBuilder::default()
                .with_title("终端 — UUX")
                .with_inner_size([960.0, 640.0])
                .with_min_inner_size([640.0, 400.0])
                .with_resizable(true),
            |ui, _| {
                if ui.input(|i| i.viewport().close_requested()) {
                    self.close_terminal();
                    return;
                }
                egui::Panel::top("terminal-toolbar")
                    .frame(egui::Frame::new().fill(SIDE).inner_margin(10))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(
                                self.active_device
                                    .as_ref()
                                    .map_or("终端", |d| d.name.as_str()),
                            );
                            ui.label(RichText::new(&self.ui_state.terminal.status).color(MUTED));
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui.button("复制屏幕").clicked() {
                                        ui.ctx().copy_text(
                                            self.ui_state.terminal.parser.screen().contents(),
                                        );
                                    }
                                    if ui
                                        .add_enabled(
                                            self.ui_state.terminal.ready,
                                            egui::Button::new("中断"),
                                        )
                                        .on_hover_text("结束当前终端会话并关闭窗口")
                                        .clicked()
                                    {
                                        self.close_terminal();
                                    }
                                    if ui
                                        .add_enabled(
                                            self.connected && !self.ui_state.terminal.ready,
                                            egui::Button::new("重新打开"),
                                        )
                                        .clicked()
                                    {
                                        self.control(json!({"op":"terminal","action":"close"}));
                                        self.ui_state.terminal = Default::default();
                                        self.start_terminal();
                                    }
                                },
                            );
                        });
                    });
                if !self.ui_state.windows.terminal {
                    return;
                }
                egui::CentralPanel::default()
                    .frame(
                        egui::Frame::new()
                            .fill(Color32::from_rgb(22, 25, 30))
                            .inner_margin(10),
                    )
                    .show(ui, |ui| self.terminal_canvas(ui));
            },
        );
    }
    fn terminal_canvas(&mut self, ui: &mut egui::Ui) {
        let font = egui::FontId::monospace(14.0);
        let unit = ui
            .painter()
            .layout_no_wrap("M".into(), font.clone(), Color32::WHITE)
            .size();
        let cell = Vec2::new(unit.x, unit.y.max(18.0));
        let (rect, response) =
            ui.allocate_exact_size(ui.available_size(), egui::Sense::click_and_drag());
        let size = [
            (rect.width() / cell.x).floor().clamp(20., 400.) as u16,
            (rect.height() / cell.y).floor().clamp(5., 200.) as u16,
        ];
        if size != self.ui_state.terminal.size {
            self.ui_state.terminal.size = size;
            self.ui_state
                .terminal
                .parser
                .screen_mut()
                .set_size(size[1], size[0]);
            if self.connected && self.ui_state.terminal.sent {
                self.control(
                    json!({"op":"terminal","action":"resize","cols":size[0],"rows":size[1]}),
                );
            }
        }
        if response.clicked() || response.drag_started() {
            response.request_focus();
        }
        let position = |p: egui::Pos2| {
            (
                (p.y - rect.top())
                    .div_euclid(cell.y)
                    .clamp(0., f32::from(size[1] - 1)) as u16,
                (p.x - rect.left())
                    .div_euclid(cell.x)
                    .clamp(0., f32::from(size[0] - 1)) as u16,
            )
        };
        if response.drag_started()
            && let Some(p) = response.interact_pointer_pos()
        {
            let p = position(p);
            self.ui_state.terminal.selection = Some((p, p));
        }
        if response.dragged()
            && let Some(p) = response.interact_pointer_pos()
            && let Some((_, end)) = &mut self.ui_state.terminal.selection
        {
            *end = position(p);
        }
        let state = &mut self.ui_state.terminal;
        let mut outbound = Vec::new();
        if ui.input(|i| i.focused)
            && (response.has_focus() || !ui.ctx().egui_wants_keyboard_input())
            && state.ready
        {
            let events = ui.input(|i| i.events.clone());
            let commits = events
                .iter()
                .filter_map(|event| match event {
                    egui::Event::Ime(egui::ImeEvent::Commit(text)) => Some(text.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>();
            for event in events {
                match event {
                    egui::Event::MouseWheel { delta, .. } if response.hovered() => {
                        let offset = state.parser.screen().scrollback() as isize
                            + if delta.y > 0. { 3 } else { -3 };
                        state
                            .parser
                            .screen_mut()
                            .set_scrollback(offset.max(0) as usize);
                    }
                    egui::Event::Key {
                        key: egui::Key::C,
                        pressed: true,
                        modifiers,
                        ..
                    } if modifiers.ctrl && modifiers.shift => {
                        let screen = state.parser.screen();
                        let text = state.selection.map_or_else(
                            || screen.contents(),
                            |(a, b)| {
                                let (start, end) = if a <= b { (a, b) } else { (b, a) };
                                screen.contents_between(
                                    start.0,
                                    start.1,
                                    end.0,
                                    end.1.saturating_add(1),
                                )
                            },
                        );
                        ui.ctx().copy_text(text);
                    }
                    egui::Event::Key {
                        key: egui::Key::V,
                        pressed: true,
                        modifiers,
                        ..
                    } if modifiers.ctrl && modifiers.shift => {
                        if let Ok(mut clipboard) = arboard::Clipboard::new()
                            && let Ok(text) = clipboard.get_text()
                            && text.len() <= 65536
                        {
                            paste(
                                &mut outbound,
                                &text,
                                state.parser.screen().bracketed_paste(),
                            );
                        }
                    }
                    egui::Event::Ime(egui::ImeEvent::Preedit { text, .. }) => {
                        state.composing = !text.is_empty()
                    }
                    egui::Event::Ime(egui::ImeEvent::Commit(text)) => {
                        state.composing = false;
                        outbound.extend_from_slice(text.as_bytes());
                    }
                    egui::Event::Text(text)
                        if !state.composing
                            && !commits.contains(&text)
                            && !ui.input(|i| i.modifiers.ctrl || i.modifiers.command) =>
                    {
                        outbound.extend_from_slice(text.as_bytes())
                    }
                    egui::Event::Paste(text) => paste(
                        &mut outbound,
                        &text,
                        state.parser.screen().bracketed_paste(),
                    ),
                    egui::Event::Key {
                        key,
                        pressed: true,
                        modifiers,
                        ..
                    } if !state.composing => outbound.extend(key_bytes(key, modifiers)),
                    _ => {}
                }
            }
        }
        let screen = state.parser.screen();
        for row in 0..size[1] {
            for col in 0..size[0] {
                let Some(value) = screen.cell(row, col) else {
                    continue;
                };
                if value.is_wide_continuation() {
                    continue;
                }
                let r = egui::Rect::from_min_size(
                    rect.min + Vec2::new(f32::from(col) * cell.x, f32::from(row) * cell.y),
                    Vec2::new(cell.x * if value.is_wide() { 2.0 } else { 1.0 }, cell.y),
                );
                let mut fg = color(value.fgcolor(), Color32::from_rgb(226, 230, 236));
                let mut bg = color(value.bgcolor(), Color32::from_rgb(22, 25, 30));
                if value.inverse() {
                    std::mem::swap(&mut fg, &mut bg);
                }
                if state.selection.is_some_and(|(a, b)| {
                    let (start, end) = if a <= b { (a, b) } else { (b, a) };
                    start <= (row, col) && (row, col) <= end
                }) {
                    bg = Color32::from_rgb(52, 82, 117);
                }
                ui.painter().rect_filled(r, 0, bg);
                ui.painter().text(
                    r.left_center(),
                    egui::Align2::LEFT_CENTER,
                    value.contents(),
                    font.clone(),
                    fg,
                );
                if value.underline() {
                    ui.painter()
                        .hline(r.x_range(), r.bottom() - 2.0, egui::Stroke::new(1.0, fg));
                }
            }
        }
        let (row, col) = screen.cursor_position();
        let cursor = egui::Rect::from_min_size(
            rect.min + Vec2::new(f32::from(col) * cell.x, f32::from(row) * cell.y),
            Vec2::new(2.0, cell.y),
        );
        if screen.scrollback() == 0 && !screen.hide_cursor() && state.ready {
            ui.painter().rect_filled(cursor, 0, Color32::WHITE);
        }
        if state.ready {
            ui.ctx().output_mut(|o| {
                o.ime = Some(egui::output::IMEOutput {
                    rect,
                    purpose: egui::IMEPurpose::Normal,
                    should_interrupt_composition: false,
                    cursor_rect: cursor,
                })
            });
        }
        if !outbound.is_empty() && outbound.len() <= 65536 {
            state.parser.screen_mut().set_scrollback(0);
            state.selection = None;
            self.control(json!({"op":"terminal","action":"input","data":base64::engine::general_purpose::STANDARD.encode(outbound)}));
        }
    }
}
fn paste(out: &mut Vec<u8>, text: &str, bracketed: bool) {
    if text.len() > 65536 {
        return;
    }
    if bracketed {
        out.extend_from_slice(b"\x1b[200~");
    }
    out.extend_from_slice(text.as_bytes());
    if bracketed {
        out.extend_from_slice(b"\x1b[201~");
    }
}
fn key_bytes(key: egui::Key, mods: egui::Modifiers) -> Vec<u8> {
    use egui::Key::*;
    if mods.ctrl && !mods.shift {
        if let Some(ch) = key
            .name()
            .bytes()
            .next()
            .filter(|b| key.name().len() == 1 && b.is_ascii_alphabetic())
        {
            return vec![ch.to_ascii_uppercase() - b'A' + 1];
        }
        if key == Space {
            return vec![0];
        }
    }
    let bytes: &[u8] = match key {
        Enter => b"\r",
        Backspace => b"\x7f",
        Tab if mods.shift => b"\x1b[Z",
        Tab => b"\t",
        Escape => b"\x1b",
        ArrowUp => b"\x1b[A",
        ArrowDown => b"\x1b[B",
        ArrowRight => b"\x1b[C",
        ArrowLeft => b"\x1b[D",
        Home => b"\x1b[H",
        End => b"\x1b[F",
        Delete => b"\x1b[3~",
        Insert => b"\x1b[2~",
        PageUp => b"\x1b[5~",
        PageDown => b"\x1b[6~",
        F1 => b"\x1bOP",
        F2 => b"\x1bOQ",
        F3 => b"\x1bOR",
        F4 => b"\x1bOS",
        F5 => b"\x1b[15~",
        F6 => b"\x1b[17~",
        F7 => b"\x1b[18~",
        F8 => b"\x1b[19~",
        F9 => b"\x1b[20~",
        F10 => b"\x1b[21~",
        F11 => b"\x1b[23~",
        F12 => b"\x1b[24~",
        _ => b"",
    };
    let mut out = Vec::new();
    if mods.alt && !bytes.is_empty() {
        out.push(27);
    }
    out.extend_from_slice(bytes);
    out
}
fn color(value: vt100::Color, default: Color32) -> Color32 {
    match value {
        vt100::Color::Default => default,
        vt100::Color::Rgb(r, g, b) => Color32::from_rgb(r, g, b),
        vt100::Color::Idx(n) => {
            const ANSI: [[u8; 3]; 16] = [
                [0, 0, 0],
                [205, 49, 49],
                [13, 188, 121],
                [229, 229, 16],
                [36, 114, 200],
                [188, 63, 188],
                [17, 168, 205],
                [229, 229, 229],
                [102, 102, 102],
                [241, 76, 76],
                [35, 209, 139],
                [245, 245, 67],
                [59, 142, 234],
                [214, 112, 214],
                [41, 184, 219],
                [255, 255, 255],
            ];
            let rgb = if n < 16 {
                ANSI[n as usize]
            } else if n >= 232 {
                [8 + (n - 232) * 10; 3]
            } else {
                let n = n - 16;
                let v = |n: u8| if n == 0 { 0 } else { 55 + 40 * n };
                [v(n / 36), v(n / 6 % 6), v(n % 6)]
            };
            Color32::from_rgb(rgb[0], rgb[1], rgb[2])
        }
    }
}

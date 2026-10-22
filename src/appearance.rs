use eframe::egui::{self, Color32, Pos2, Rect, Response, Sense, Stroke, Vec2};

pub const SIDE: Color32 = Color32::from_rgb(240, 245, 248);
pub const BORDER: Color32 = Color32::from_rgb(230, 230, 230);
pub const SELECTED: Color32 = Color32::from_rgb(226, 231, 233);

/// Native-style entry: a quiet outline, with focus indicated on the bottom edge.
/// Keep egui's editor for selection, IME, accessibility and keyboard navigation.
pub struct TextField<'a>(egui::TextEdit<'a>);
impl<'a> TextField<'a> {
    pub fn new(text: &'a mut String) -> Self {
        Self(egui::TextEdit::singleline(text).align(egui::Align2::LEFT_CENTER))
    }
    pub fn hint_text(mut self, text: impl Into<egui::WidgetText>) -> Self {
        self.0 = self.0.hint_text(text);
        self
    }
    pub fn id_salt(mut self, id: impl egui::AsIdSalt) -> Self {
        self.0 = self.0.id_salt(id);
        self
    }
    pub fn char_limit(mut self, limit: usize) -> Self {
        self.0 = self.0.char_limit(limit);
        self
    }
    pub fn password(mut self, password: bool) -> Self {
        self.0 = self.0.password(password);
        self
    }
    pub fn font(mut self, font: impl Into<egui::FontSelection>) -> Self {
        self.0 = self.0.font(font);
        self
    }
    #[cfg(test)]
    pub fn desired_width(mut self, width: f32) -> Self {
        self.0 = self.0.desired_width(width);
        self
    }
    pub fn min_size(mut self, size: Vec2) -> Self {
        self.0 = self.0.min_size(size);
        self
    }
    pub fn margin(mut self, margin: impl Into<egui::Margin>) -> Self {
        self.0 = self.0.margin(margin);
        self
    }
    pub fn show(self, ui: &mut egui::Ui) -> egui::text_edit::TextEditOutput {
        let old_selection = ui.visuals().selection;
        ui.visuals_mut().selection.bg_fill = super::BLUE;
        ui.visuals_mut().selection.stroke.color = Color32::WHITE;
        let output = self.0.show(ui);
        ui.visuals_mut().selection = old_selection;
        // Paint inside the existing frame: preserve the editor's allocated size.
        if output.response.has_focus() {
            let rect = output.response.rect;
            ui.painter()
                .rect_stroke(rect, 3, Stroke::new(1.0, BORDER), egui::StrokeKind::Inside);
            ui.painter().hline(
                rect.x_range(),
                rect.bottom() - 1.0,
                Stroke::new(2.0, super::BLUE),
            );
        }
        output
    }
}
impl egui::Widget for TextField<'_> {
    fn ui(self, ui: &mut egui::Ui) -> Response {
        self.show(ui).response.response
    }
}

/// Shared hover/press feedback for our painted controls. Disabled controls must
/// never look actionable; egui handles their reduced opacity.
pub fn hover_background(ui: &egui::Ui, response: &Response, rect: Rect) {
    if response.enabled() && (response.hovered() || response.is_pointer_button_down_on()) {
        ui.painter().rect_filled(
            rect,
            3,
            if response.is_pointer_button_down_on() {
                Color32::from_rgb(210, 221, 229)
            } else {
                SELECTED
            },
        );
        response
            .clone()
            .on_hover_cursor(egui::CursorIcon::PointingHand);
    }
}

pub fn primary(ui: &mut egui::Ui, label: &str, size: Vec2) -> Response {
    ui.scope(|ui| {
        let widgets = &mut ui.visuals_mut().widgets;
        for (widget, fill) in [
            (&mut widgets.inactive, super::BLUE),
            (&mut widgets.hovered, Color32::from_rgb(79, 147, 255)),
            (&mut widgets.active, Color32::from_rgb(32, 107, 227)),
        ] {
            widget.weak_bg_fill = fill;
            widget.bg_fill = fill;
            widget.bg_stroke = Stroke::NONE;
            widget.fg_stroke.color = Color32::WHITE;
        }
        if !ui.is_enabled() {
            ui.visuals_mut().widgets.inactive.weak_bg_fill = Color32::from_rgb(203, 205, 207);
            // The disabled fill is already gray. Do not fade it a second time.
            ui.visuals_mut().disabled_alpha = 1.0;
            if ui.is_visible() {
                ui.set_opacity(1.0);
            }
        }
        // Menu UIs align ordinary button text to the left. Primary actions keep
        // the same centered label in a popup as in a form or dialog.
        ui.add(
            egui::Button::new((
                egui::Atom::grow(),
                egui::RichText::new(label).color(Color32::WHITE),
                egui::Atom::grow(),
            ))
            .min_size(size),
        )
    })
    .inner
}

pub fn flat_button(ui: &mut egui::Ui, label: &str, size: Vec2) -> Response {
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    hover_background(ui, &response, rect);
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        label,
        egui::FontId::proportional(13.0),
        super::INK,
    );
    response
}

pub fn icon_label(ui: &mut egui::Ui, icon: Icon, label: &str, size: Vec2) -> Response {
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    hover_background(ui, &response, rect);
    let text = ui.painter().layout_no_wrap(
        label.to_owned(),
        egui::FontId::proportional(13.0),
        super::INK,
    );
    let left = rect.center().x - (text.size().x + 22.0) * 0.5;
    draw(
        ui.painter(),
        Rect::from_center_size(egui::pos2(left + 7.0, rect.center().y), Vec2::splat(14.0)),
        icon,
        super::INK,
    );
    ui.painter().galley(
        egui::pos2(left + 22.0, rect.center().y - text.size().y * 0.5),
        text,
        super::INK,
    );
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, ui.is_enabled(), label));
    response
}

pub fn screen_tab(ui: &mut egui::Ui, title: &str, selected: bool) -> (Response, Response) {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(190.0, 32.0), Sense::click());
    let corners = egui::CornerRadius {
        nw: 8,
        ne: 8,
        sw: 0,
        se: 0,
    };
    if selected || response.hovered() {
        ui.painter()
            .rect_filled(rect, corners, if selected { Color32::WHITE } else { SIDE });
    }
    draw(
        ui.painter(),
        Rect::from_center_size(rect.left_center() + Vec2::new(15.0, 0.0), Vec2::splat(16.0)),
        Icon::Fullscreen,
        super::INK,
    );
    let mut job = egui::text::LayoutJob::simple_singleline(
        title.into(),
        egui::FontId::proportional(12.0),
        super::INK,
    );
    job.wrap.max_width = rect.width() - 60.0;
    job.wrap.max_rows = 1;
    let text = ui.painter().layout_job(job);
    ui.painter().galley(
        rect.left_center() + Vec2::new(29.0, -text.size().y * 0.5),
        text,
        super::INK,
    );
    let close_rect = Rect::from_center_size(
        rect.right_center() - Vec2::new(15.0, 0.0),
        Vec2::splat(22.0),
    );
    let close = ui.interact(close_rect, response.id.with("close"), Sense::click());
    if response.hovered() || close.hovered() {
        hover_background(ui, &close, close_rect);
        draw(
            ui.painter(),
            close_rect.shrink(3.0),
            Icon::Close,
            super::INK,
        );
    }
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Tab, ui.is_enabled(), title));
    close.widget_info(|| {
        egui::WidgetInfo::labeled(egui::Role::Button, ui.is_enabled(), "关闭屏幕视图")
    });
    (
        response
            .on_hover_text(title)
            .on_hover_cursor(egui::CursorIcon::PointingHand),
        close.on_hover_text("关闭此屏幕视图，不移除远端显示器"),
    )
}

/// A recent partner entry has three separate hit targets, like the native list.
pub fn history_chip(ui: &mut egui::Ui, id: &str, favorite: bool) -> (Response, Response, Response) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(132.0, 28.0), Sense::hover());
    ui.painter().rect_filled(rect, 3, Color32::WHITE);
    ui.painter()
        .rect_stroke(rect, 3, Stroke::new(1.0, BORDER), egui::StrokeKind::Inside);
    let star_rect = Rect::from_min_max(rect.min, Pos2::new(rect.left() + 28.0, rect.bottom()));
    let close_rect = Rect::from_min_max(Pos2::new(rect.right() - 26.0, rect.top()), rect.max);
    let body_rect = Rect::from_min_max(star_rect.right_top(), close_rect.left_bottom());
    let star = ui.interact(
        star_rect,
        ui.scope_id().with((id, "favorite")),
        Sense::click(),
    );
    let close = ui.interact(
        close_rect,
        ui.scope_id().with((id, "remove")),
        Sense::click(),
    );
    let body = ui.interact(
        body_rect,
        ui.scope_id().with((id, "select")),
        Sense::click(),
    );
    for response in [&star, &close, &body] {
        hover_background(ui, response, response.rect.shrink(1.0));
    }
    draw(
        ui.painter(),
        Rect::from_center_size(star_rect.center(), Vec2::splat(14.0)),
        Icon::StarOutline,
        if favorite { super::BLUE } else { super::MUTED },
    );
    draw(
        ui.painter(),
        Rect::from_center_size(close_rect.center(), Vec2::splat(12.0)),
        Icon::Close,
        super::MUTED,
    );
    ui.painter().text(
        body_rect.center(),
        egui::Align2::CENTER_CENTER,
        id,
        egui::FontId::proportional(12.0),
        super::INK,
    );
    body.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, ui.is_enabled(), id));
    star.widget_info(|| {
        egui::WidgetInfo::selected(egui::Role::CheckBox, ui.is_enabled(), favorite, "收藏设备")
    });
    close.widget_info(|| {
        egui::WidgetInfo::labeled(egui::Role::Button, ui.is_enabled(), "删除此条记录")
    });
    (
        body.on_hover_cursor(egui::CursorIcon::PointingHand),
        star.on_hover_text(if favorite {
            "取消收藏"
        } else {
            "收藏设备"
        })
        .on_hover_cursor(egui::CursorIcon::PointingHand),
        close
            .on_hover_text("删除此条记录")
            .on_hover_cursor(egui::CursorIcon::PointingHand),
    )
}

#[derive(Clone, Copy, Debug)]
pub enum Icon {
    Back,
    Up,
    Collapse,
    Refresh,
    Account,
    Menu,
    Minimize,
    Maximize,
    Close,
    Devices,
    All,
    Assist,
    AssistGroup,
    Star,
    StarOutline,
    Settings,
    Chevron,
    Down,
    Info,
    Folder,
    Sound,
    Microphone,
    MicrophoneMuted,
    Picture,
    Shield,
    Usb,
    Grid,
    Keyboard,
    Transfer,
    Fullscreen,
    Exit,
    Pause,
    Play,
    Trash,
    FolderAdd,
    Edit,
    Terminal,
    Power,
    Control,
}

/// Diagnostic-only contact sheet, using the same painter and sizes as the UI.
pub fn gallery(ctx: &egui::Context, open: &mut bool) {
    use Icon::*;
    let icons = [
        Back,
        Up,
        Collapse,
        Refresh,
        Account,
        Menu,
        Minimize,
        Maximize,
        Close,
        Devices,
        All,
        Assist,
        AssistGroup,
        Star,
        StarOutline,
        Settings,
        Chevron,
        Down,
        Info,
        Folder,
        Sound,
        Microphone,
        MicrophoneMuted,
        Picture,
        Shield,
        Usb,
        Grid,
        Keyboard,
        Transfer,
        Fullscreen,
        Exit,
        Pause,
        Play,
        Trash,
        FolderAdd,
        Edit,
        Terminal,
        Power,
        Control,
    ];
    egui::Window::new("UI icons · 16 / 20 / 24 pt")
        .open(open)
        .resizable(false)
        .collapsible(false)
        .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
        .show(ctx, |ui| {
            egui::Grid::new("icon-gallery")
                .num_columns(4)
                .spacing([24.0, 8.0])
                .show(ui, |ui| {
                    for (i, icon) in icons.into_iter().enumerate() {
                        ui.vertical(|ui| {
                            ui.label(format!("{icon:?}"));
                            ui.horizontal(|ui| {
                                for size in [16.0, 20.0, 24.0] {
                                    let (rect, _) =
                                        ui.allocate_exact_size(Vec2::splat(28.0), Sense::hover());
                                    draw(
                                        ui.painter(),
                                        Rect::from_center_size(rect.center(), Vec2::splat(size)),
                                        icon,
                                        if matches!(icon, Star | Assist | All) {
                                            super::BLUE
                                        } else {
                                            super::INK
                                        },
                                    );
                                }
                            });
                        });
                        if i % 4 == 3 {
                            ui.end_row();
                        }
                    }
                });
        });
}

/// A single fill and a one-physical-pixel alpha fringe, without seams between
/// triangles. The outline must be star-shaped around `center` (star/bookmark).
fn feathered_fill(p: &egui::Painter, center: Pos2, points: &[Pos2], color: Color32) {
    let mut mesh = egui::Mesh::default();
    mesh.colored_vertex(center, color);
    let half_pixel = 0.5 / p.ctx().pixels_per_point();
    for (i, point) in points.iter().enumerate() {
        let previous = (*point - points[(i + points.len() - 1) % points.len()]).normalized();
        let next = (points[(i + 1) % points.len()] - *point).normalized();
        let a = Vec2::new(previous.y, -previous.x);
        let b = Vec2::new(next.y, -next.x);
        let normal = (a + b).normalized();
        let offset = normal * (half_pixel / normal.dot(b).max(0.25));
        mesh.colored_vertex(*point - offset, color);
        mesh.colored_vertex(*point + offset, Color32::TRANSPARENT);
    }
    for i in 0..points.len() as u32 {
        let a = 1 + 2 * i;
        let b = 1 + 2 * ((i + 1) % points.len() as u32);
        mesh.add_triangle(0, a, b);
        mesh.add_triangle(a, a + 1, b);
        mesh.add_triangle(b, a + 1, b + 1);
    }
    p.add(mesh);
}

pub fn draw(p: &egui::Painter, rect: Rect, icon: Icon, color: Color32) {
    let origin = rect.center() - Vec2::splat(rect.width().min(rect.height()) / 2.0);
    let scale = rect.width().min(rect.height()) / 24.0;
    let point = |x: f32, y: f32| origin + Vec2::new(x, y) * scale;
    let stroke = Stroke::new(1.15, color);
    let line = |a: (f32, f32), b: (f32, f32)| {
        p.line_segment([point(a.0, a.1), point(b.0, b.1)], stroke);
    };
    let path = |v: &[(f32, f32)]| {
        p.add(egui::Shape::line(
            v.iter().map(|&(x, y)| point(x, y)).collect(),
            stroke,
        ));
    };
    let curve = |v: [(f32, f32); 4]| {
        let bezier = egui::epaint::CubicBezierShape::from_points_stroke(
            v.map(|(x, y)| point(x, y)),
            false,
            Color32::TRANSPARENT,
            Stroke::NONE,
        );
        p.add(egui::Shape::line(
            bezier.flatten(Some(0.08 / p.ctx().pixels_per_point())),
            stroke,
        ));
    };
    let box_at = |x: f32, y: f32, w: f32, h: f32, radius: u8| {
        p.rect_stroke(
            Rect::from_min_size(point(x, y), Vec2::new(w, h) * scale),
            radius,
            stroke,
            egui::StrokeKind::Inside,
        );
    };
    match icon {
        Icon::Microphone | Icon::MicrophoneMuted => {
            box_at(9., 3., 6., 12., 3);
            line((6., 10.), (6., 12.));
            curve([(6., 12.), (6., 20.), (18., 20.), (18., 12.)]);
            line((18., 12.), (18., 10.));
            line((12., 18.), (12., 22.));
            line((8., 22.), (16., 22.));
            if matches!(icon, Icon::MicrophoneMuted) {
                line((4., 3.), (21., 21.));
            }
        }
        Icon::Picture => {
            box_at(5., 3., 16., 17., 2);
            path(&[(3., 6.), (2., 6.), (2., 22.), (17., 22.)]);
            p.circle_stroke(point(10., 8.), 1.8 * scale, stroke);
            path(&[(6., 17.), (11., 12.), (14., 15.), (17., 11.), (20., 15.)]);
        }
        Icon::Shield => {
            p.add(egui::Shape::closed_line(
                vec![
                    point(12., 2.),
                    point(21., 6.),
                    point(20., 14.),
                    point(17., 19.),
                    point(12., 23.),
                    point(7., 19.),
                    point(4., 14.),
                    point(3., 6.),
                ],
                stroke,
            ));
            path(&[(12., 7.), (10., 12.), (14., 12.), (12., 17.)]);
        }
        Icon::Usb => {
            line((12., 20.), (12., 3.));
            path(&[(9., 6.), (12., 3.), (15., 6.)]);
            path(&[(12., 16.), (6., 12.), (6., 8.)]);
            path(&[(12., 13.), (18., 9.), (18., 6.)]);
            p.circle_stroke(point(12., 21.), 1.8 * scale, stroke);
            p.circle_stroke(point(6., 6.), 1.8 * scale, stroke);
            box_at(16.5, 3., 3., 3., 0);
        }
        Icon::Grid => {
            for (x, y) in [(3., 3.), (14., 3.), (3., 14.), (14., 14.)] {
                box_at(x, y, 7., 7., 1);
            }
        }
        Icon::Control => {
            box_at(3., 3., 18., 7., 3);
            box_at(3., 14., 18., 7., 3);
            p.circle_filled(point(8., 6.5), 1.6 * scale, color);
            p.circle_filled(point(16., 17.5), 1.6 * scale, color);
        }
        Icon::Terminal => {
            box_at(3., 5., 18., 15., 2);
            path(&[(6., 9.), (9., 12.), (6., 15.)]);
            line((12., 15.), (17., 15.));
        }
        Icon::Power => {
            line((12., 2.), (12., 12.));
            curve([(7., 5.), (0., 10.), (4., 21.), (12., 21.)]);
            curve([(12., 21.), (20., 21.), (24., 10.), (17., 5.)]);
        }
        Icon::Edit => {
            path(&[
                (5., 15.),
                (16., 4.),
                (20., 8.),
                (9., 19.),
                (4., 20.),
                (5., 15.),
            ]);
            line((13., 7.), (17., 11.));
        }
        Icon::Back => {
            line((19., 12.), (5., 12.));
            path(&[(11., 6.), (5., 12.), (11., 18.)]);
        }
        Icon::Up => {
            line((12., 20.), (12., 4.));
            path(&[(6., 10.), (12., 4.), (18., 10.)]);
        }
        Icon::Collapse => path(&[(5., 15.), (12., 8.), (19., 15.)]),
        Icon::Pause => {
            line((8., 5.), (8., 19.));
            line((16., 5.), (16., 19.));
        }
        Icon::Play => path(&[(7., 4.), (19., 12.), (7., 20.), (7., 4.)]),
        Icon::Trash => {
            path(&[(6., 7.), (7., 21.), (17., 21.), (18., 7.)]);
            line((4., 6.), (20., 6.));
            path(&[(9., 6.), (9., 3.), (15., 3.), (15., 6.)]);
            line((10., 10.), (10., 17.));
            line((14., 10.), (14., 17.));
        }
        Icon::FolderAdd => {
            path(&[
                (3., 19.),
                (3., 5.),
                (10., 5.),
                (12., 8.),
                (21., 8.),
                (21., 19.),
                (3., 19.),
            ]);
            line((9., 14.), (17., 14.));
            line((13., 10.), (13., 18.));
        }
        Icon::Refresh => {
            let points = (0..26)
                .map(|i| {
                    let a = 0.4 + i as f32 * 0.215;
                    point(12. + 8. * a.cos(), 12. + 8. * a.sin())
                })
                .collect();
            p.add(egui::Shape::line(points, stroke));
            path(&[(16., 7.), (20., 9.), (21., 4.)]);
        }
        Icon::Account => {
            p.circle_stroke(point(12., 6.), 3. * scale, stroke);
            line((5., 13.), (19., 13.));
            curve([(5., 13.), (5., 24.), (19., 24.), (19., 13.)]);
        }
        Icon::Menu => {
            for y in [6., 12., 18.] {
                line((3., y), (21., y));
            }
        }
        Icon::Minimize => line((5., 12.), (19., 12.)),
        Icon::Maximize => box_at(6., 6., 12., 12., 0),
        Icon::Close => {
            line((6., 6.), (18., 18.));
            line((18., 6.), (6., 18.));
        }
        Icon::Devices => {
            box_at(2., 5., 15., 10., 1);
            box_at(14., 10., 8., 9., 1);
            line((6., 19.), (11., 19.));
            line((9., 15.), (9., 19.));
        }
        Icon::All => {
            for (x, y, opacity) in [
                (6.5, 6.5, 0.7),
                (17.5, 6.5, 1.0),
                (6.5, 17.5, 1.0),
                (17.5, 17.5, 0.7),
            ] {
                // Leave room for both antialiased edges at small icon sizes.
                p.circle_filled(point(x, y), 4.2 * scale, color.gamma_multiply(opacity));
            }
        }
        Icon::Assist => {
            p.rect_filled(
                Rect::from_min_max(point(2., 10.), point(12., 21.)),
                2,
                color,
            );
            p.rect_filled(
                Rect::from_min_max(point(11., 2.), point(22., 13.)),
                2,
                color,
            );
            path(&[(3., 7.), (3., 3.), (8., 3.)]);
            path(&[(16., 20.), (21., 20.), (21., 16.)]);
        }
        Icon::AssistGroup => {
            box_at(2., 3., 8., 6., 1);
            box_at(14., 14., 8., 6., 1);
            path(&[(14., 4.), (21., 4.), (21., 9.)]);
            path(&[(2., 14.), (2., 20.), (9., 20.)]);
        }
        Icon::StarOutline => {
            let points = (0..10)
                .map(|i| {
                    let angle =
                        -std::f32::consts::FRAC_PI_2 + i as f32 * std::f32::consts::PI / 5.0;
                    let radius = if i % 2 == 0 { 10.0 } else { 4.5 };
                    point(12.0 + radius * angle.cos(), 12.0 + radius * angle.sin())
                })
                .collect();
            p.add(egui::Shape::closed_line(points, stroke));
        }
        Icon::Star => {
            feathered_fill(
                p,
                point(12., 10.),
                &[
                    point(5., 1.),
                    point(19., 1.),
                    point(20., 1.27),
                    point(20.73, 2.),
                    point(21., 3.),
                    point(21., 23.),
                    point(12., 17.),
                    point(3., 23.),
                    point(3., 3.),
                    point(3.27, 2.),
                    point(4., 1.27),
                ],
                color,
            );
            let pts: Vec<_> = (0..10)
                .map(|i| {
                    let a = -std::f32::consts::FRAC_PI_2 + i as f32 * std::f32::consts::PI / 5.;
                    let r = if i % 2 == 0 { 6.5 } else { 3.0 };
                    point(12. + r * a.cos(), 10.5 + r * a.sin())
                })
                .collect();
            feathered_fill(p, point(12., 10.5), &pts, Color32::WHITE);
        }
        Icon::Settings => {
            p.circle_stroke(point(12., 12.), 7. * scale, stroke);
            p.circle_stroke(point(12., 12.), 2.5 * scale, stroke);
            for i in 0..8 {
                let a = i as f32 * std::f32::consts::TAU / 8.;
                line(
                    (12. + 7. * a.cos(), 12. + 7. * a.sin()),
                    (12. + 10. * a.cos(), 12. + 10. * a.sin()),
                );
            }
        }
        Icon::Chevron => path(&[(9., 6.), (15., 12.), (9., 18.)]),
        Icon::Down => path(&[(5., 9.), (12., 16.), (19., 9.)]),
        Icon::Info => {
            p.circle_stroke(point(12., 12.), 9. * scale, stroke);
            line((12., 11.), (12., 17.));
            p.circle_filled(point(12., 7.), 0.9 * scale, color);
        }
        Icon::Folder => {
            path(&[
                (3., 19.),
                (3., 5.),
                (10., 5.),
                (12., 8.),
                (21., 8.),
                (21., 19.),
                (3., 19.),
            ]);
        }
        Icon::Sound => {
            path(&[
                (3., 9.),
                (7., 9.),
                (12., 4.),
                (12., 20.),
                (7., 15.),
                (3., 15.),
                (3., 9.),
            ]);
            curve([(16., 8.), (20., 10.), (20., 14.), (16., 16.)]);
        }
        Icon::Keyboard => {
            box_at(2., 5., 20., 14., 2);
            for x in [6., 10., 14., 18.] {
                for y in [9., 12.] {
                    p.circle_filled(point(x, y), 0.65 * scale, color);
                }
            }
            line((7., 16.), (17., 16.));
        }
        Icon::Transfer => {
            box_at(2., 3., 13., 12., 2);
            path(&[(10., 12.), (21., 12.), (17., 8.)]);
            path(&[(14., 17.), (4., 17.), (8., 21.)]);
        }
        Icon::Fullscreen => {
            for (x, y, dx, dy) in [
                (3., 3., 1., 1.),
                (21., 3., -1., 1.),
                (3., 21., 1., -1.),
                (21., 21., -1., -1.),
            ] {
                path(&[(x + 6. * dx, y), (x, y), (x, y + 6. * dy)]);
            }
        }
        Icon::Exit => {
            path(&[(10., 3.), (4., 3.), (4., 21.), (10., 21.)]);
            line((9., 12.), (22., 12.));
            path(&[(17., 7.), (22., 12.), (17., 17.)]);
        }
    }
}

pub fn button(ui: &mut egui::Ui, icon: Icon, title: &str) -> Response {
    sized_icon_button(ui, icon, title, Vec2::new(38., 32.))
}
pub fn sized_icon_button(ui: &mut egui::Ui, icon: Icon, title: &str, size: Vec2) -> Response {
    let (r, response) = ui.allocate_exact_size(size, Sense::click());
    hover_background(ui, &response, r);
    draw(
        ui.painter(),
        Rect::from_center_size(r.center(), Vec2::splat(18.)),
        icon,
        if ui.is_enabled() {
            super::INK
        } else {
            Color32::from_gray(170)
        },
    );
    response.on_hover_text(title)
}

pub fn logo(p: &egui::Painter, center: Pos2, size: f32) {
    crate::brand::paint(p, center, size);
}

pub fn window_buttons(ui: &mut egui::Ui) {
    if button(ui, Icon::Close, "关闭").clicked() {
        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
    }
    if button(ui, Icon::Maximize, "最大化 / 还原").clicked() {
        let maximized = ui.input(|i| i.viewport().maximized.unwrap_or(false));
        ui.ctx()
            .send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized));
    }
    if button(ui, Icon::Minimize, "最小化").clicked() {
        ui.ctx()
            .send_viewport_cmd(egui::ViewportCommand::Minimized(true));
    }
}

pub fn toggle(ui: &mut egui::Ui, value: &mut bool) -> Response {
    let (r, mut response) = ui.allocate_exact_size(Vec2::new(40., 20.), Sense::click());
    if response.clicked() {
        *value = !*value;
        response.mark_changed();
    }
    let color = if *value {
        if response.hovered() {
            Color32::from_rgb(79, 147, 255)
        } else {
            super::BLUE
        }
    } else if response.hovered() {
        Color32::from_rgb(225, 232, 237)
    } else {
        Color32::from_rgb(247, 248, 249)
    };
    ui.painter().rect_filled(r, 11, color);
    if !*value {
        ui.painter().rect_stroke(
            r,
            11,
            Stroke::new(
                1.0,
                if response.hovered() {
                    Color32::from_rgb(120, 140, 158)
                } else {
                    Color32::from_gray(157)
                },
            ),
            egui::StrokeKind::Inside,
        );
    }
    ui.painter().circle_filled(
        egui::pos2(
            if *value {
                r.right() - 10.
            } else {
                r.left() + 10.
            },
            r.center().y,
        ),
        6.,
        if *value {
            Color32::WHITE
        } else {
            Color32::from_gray(100)
        },
    );
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn painted_controls_have_hover_feedback_and_disabled_controls_do_not() {
        for kind in 0..11 {
            for enabled in [true, false] {
                let ctx = egui::Context::default();
                super::super::setup(&ctx);
                let mut center = Pos2::ZERO;
                let mut frame = |hover: bool| {
                    let output = ctx.run_ui(
                        egui::RawInput {
                            screen_rect: Some(Rect::from_min_size(
                                Pos2::ZERO,
                                Vec2::new(400.0, 300.0),
                            )),
                            events: vec![egui::Event::PointerMoved(if hover {
                                center
                            } else {
                                egui::pos2(350.0, 250.0)
                            })],
                            ..Default::default()
                        },
                        |ui| {
                            center = ui
                                .add_enabled_ui(enabled, |ui| match kind {
                                    0 => button(ui, Icon::Refresh, "Refresh"),
                                    1 => primary(ui, "Connect", Vec2::new(112.0, 30.0)),
                                    2 => toggle(ui, &mut true),
                                    3 => toggle(ui, &mut false),
                                    4 => ui.button("Standard"),
                                    5 => flat_button(ui, "More", Vec2::new(32.0, 28.0)),
                                    6 => icon_label(
                                        ui,
                                        Icon::Pause,
                                        "全部暂停",
                                        Vec2::new(116.0, 28.0),
                                    ),
                                    7 => screen_tab(ui, "显示器", false).0,
                                    8 => history_chip(ui, "957857075", false).0,
                                    9 => history_chip(ui, "957857075", false).1,
                                    _ => history_chip(ui, "957857075", false).2,
                                })
                                .inner
                                .rect
                                .center();
                        },
                    );
                    let shape_data = format!("{:?}", output.shapes);
                    let cursor = output.platform_output.cursor_icon;
                    output.drop_without_applying_deltas();
                    (shape_data, cursor)
                };
                frame(false);
                let idle = frame(false);
                let hovered = frame(true);
                if enabled {
                    assert_ne!(idle.0, hovered.0, "control {kind} has no hover paint");
                    assert_eq!(hovered.1, egui::CursorIcon::PointingHand);
                } else {
                    assert_eq!(
                        idle.0, hovered.0,
                        "disabled control {kind} changes on hover"
                    );
                    assert_ne!(hovered.1, egui::CursorIcon::PointingHand);
                }
            }
        }
    }

    #[test]
    fn recent_partner_actions_have_independent_hit_targets() {
        let ctx = egui::Context::default();
        super::super::setup(&ctx);
        let render = |events| {
            let mut result = None;
            ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(400.0, 100.0))),
                    events,
                    ..Default::default()
                },
                |ui| {
                    result = Some(history_chip(ui, "957857075", false));
                },
            )
            .drop_without_applying_deltas();
            let (select, favorite, remove) = result.unwrap();
            [select, favorite, remove]
        };
        let targets = render(vec![]).map(|response| response.rect.center());
        for (selected, pos) in targets.into_iter().enumerate() {
            render(vec![egui::Event::PointerMoved(pos)]);
            let button = |pressed| egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: Default::default(),
            };
            render(vec![button(true)]);
            for (index, response) in render(vec![button(false)]).into_iter().enumerate() {
                assert_eq!(response.clicked(), index == selected);
            }
        }
    }

    #[test]
    fn closing_screen_tab_does_not_select_its_body() {
        let ctx = egui::Context::default();
        super::super::setup(&ctx);
        let render = |events| {
            let mut result = None;
            ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(400.0, 100.0))),
                    events,
                    ..Default::default()
                },
                |ui| {
                    result = Some(screen_tab(ui, "很长的显示器名称", false));
                },
            )
            .drop_without_applying_deltas();
            result.unwrap()
        };
        let (tab, close) = render(vec![]);
        assert!(tab.rect.contains_rect(close.rect));
        for (position, closing) in [(tab.rect.center(), false), (close.rect.center(), true)] {
            render(vec![egui::Event::PointerMoved(position)]);
            let button = |pressed| egui::Event::PointerButton {
                pos: position,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: Default::default(),
            };
            render(vec![button(true)]);
            let (tab, close) = render(vec![button(false)]);
            assert_eq!(close.clicked(), closing);
            assert_eq!(tab.clicked(), !closing);
        }
    }
}

use eframe::egui::{self, Color32, Pos2, Rect, Response, Sense, Stroke, Vec2};

pub const SIDE: Color32 = Color32::from_rgb(240, 245, 248);
pub const BORDER: Color32 = Color32::from_rgb(230, 230, 230);
pub const SELECTED: Color32 = Color32::from_rgb(226, 231, 233);

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
        }
        ui.add(egui::Button::new(egui::RichText::new(label).color(Color32::WHITE)).min_size(size))
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

#[derive(Clone, Copy)]
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
    Star,
    Settings,
    Chevron,
    Down,
    Info,
    Folder,
    Sound,
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
    let box_at = |x: f32, y: f32, w: f32, h: f32, radius: u8| {
        p.rect_stroke(
            Rect::from_min_size(point(x, y), Vec2::new(w, h) * scale),
            radius,
            stroke,
            egui::StrokeKind::Inside,
        );
    };
    match icon {
        Icon::Terminal => {
            box_at(3., 5., 18., 15., 2);
            path(&[(6., 9.), (9., 12.), (6., 15.)]);
            line((12., 15.), (17., 15.));
        }
        Icon::Power => {
            line((12., 2.), (12., 12.));
            path(&[
                (7., 5.),
                (4., 8.),
                (3., 12.),
                (5., 17.),
                (9., 20.),
                (15., 20.),
                (19., 17.),
                (21., 12.),
                (20., 8.),
                (17., 5.),
            ]);
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
            path(&[
                (5., 13.),
                (19., 13.),
                (18., 18.),
                (15., 21.),
                (9., 21.),
                (6., 18.),
                (5., 13.),
            ]);
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
            for (x, y) in [(7., 7.), (17., 7.), (7., 17.), (17., 17.)] {
                p.circle_filled(point(x, y), 4.6 * scale, color);
            }
        }
        Icon::Assist => {
            box_at(2., 4., 12., 10., 2);
            box_at(11., 11., 11., 10., 2);
            line((4., 18.), (8., 18.));
        }
        Icon::Star => {
            let pts = (0..10)
                .map(|i| {
                    let a = -std::f32::consts::FRAC_PI_2 + i as f32 * std::f32::consts::PI / 5.;
                    let r = if i % 2 == 0 { 10. } else { 4.5 };
                    point(12. + r * a.cos(), 12. + r * a.sin())
                })
                .collect();
            p.add(egui::Shape::convex_polygon(pts, color, Stroke::NONE));
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
            path(&[(16., 8.), (18., 11.), (18., 13.), (16., 16.)]);
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
    let (r, response) = ui.allocate_exact_size(Vec2::new(38., 32.), Sense::click());
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
    let s = size / 24.;
    let pt = |x: f32, y: f32| center + Vec2::new(x - 12., y - 12.) * s;
    for (v, c) in [
        (
            vec![(12., 1.), (12., 12.), (22., 12.)],
            Color32::from_rgb(0, 101, 255),
        ),
        (
            vec![(12., 1.), (5., 12.), (12., 12.)],
            Color32::from_rgb(62, 185, 255),
        ),
        (
            vec![(2., 12.), (12., 12.), (12., 23.)],
            Color32::from_rgb(0, 215, 224),
        ),
        (
            vec![(12., 12.), (22., 12.), (12., 23.)],
            Color32::from_rgb(0, 128, 236),
        ),
    ] {
        p.add(egui::Shape::convex_polygon(
            v.into_iter().map(|(x, y)| pt(x, y)).collect(),
            c,
            Stroke::NONE,
        ));
    }
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
    let (r, mut response) = ui.allocate_exact_size(Vec2::new(40., 22.), Sense::click());
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
                r.right() - 11.
            } else {
                r.left() + 11.
            },
            r.center().y,
        ),
        7.,
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
        for kind in 0..6 {
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
                                    _ => flat_button(ui, "More", Vec2::new(32.0, 28.0)),
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
}

use eframe::egui::{self, Color32, Pos2, Stroke, Vec2};
use std::sync::{Arc, OnceLock};

pub fn icon() -> Arc<egui::IconData> {
    static ICON: OnceLock<Arc<egui::IconData>> = OnceLock::new();
    ICON.get_or_init(|| {
        Arc::new(
            eframe::icon_data::from_png_bytes(include_bytes!("../assets/uux.png"))
                .expect("embedded UUX icon is a valid PNG"),
        )
    })
    .clone()
}

/// The two cubic paths from assets/uux.svg (selected concept 08), painted as
/// vectors so title bars stay sharp at fractional display scales.
pub fn paint(painter: &egui::Painter, center: Pos2, size: f32) {
    let scale = size / 64.0;
    let point = |x: f32, y: f32| center + Vec2::new(x - 32.0, y - 32.0) * scale;
    for (color, curves) in [
        (
            Color32::from_rgb(52, 120, 246),
            [
                [(32., 32.), (24., 20.), (20., 16.), (14., 16.)],
                [(14., 16.), (7., 16.), (4., 23.), (4., 32.)],
                [(4., 32.), (4., 41.), (7., 48.), (14., 48.)],
                [(14., 48.), (20., 48.), (24., 44.), (32., 32.)],
            ],
        ),
        (
            Color32::from_rgb(32, 198, 213),
            [
                [(32., 32.), (40., 20.), (44., 16.), (50., 16.)],
                [(50., 16.), (57., 16.), (60., 23.), (60., 32.)],
                [(60., 32.), (60., 41.), (57., 48.), (50., 48.)],
                [(50., 48.), (44., 48.), (40., 44.), (32., 32.)],
            ],
        ),
    ] {
        let mut points = vec![point(32.0, 32.0)];
        for curve in curves {
            let curve = egui::epaint::CubicBezierShape::from_points_stroke(
                curve.map(|(x, y)| point(x, y)),
                false,
                Color32::TRANSPARENT,
                Stroke::NONE,
            );
            points.extend(
                curve
                    .flatten(Some(0.05 / painter.ctx().pixels_per_point()))
                    .into_iter()
                    .skip(1),
            );
        }
        painter.add(egui::Shape::line(points, Stroke::new(7.0 * scale, color)));
        painter.circle_filled(center, 3.5 * scale, color);
    }
}

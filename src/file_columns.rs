use super::*;

#[derive(Clone, Copy, Default)]
pub(super) struct Columns<const N: usize> {
    widths: Option<[f32; N]>,
}

pub(super) const FILE_RATIOS: [f32; 4] = [0.29, 0.33, 0.15, 0.23];
pub(super) const FILE_MIN: [f32; 4] = [100.0, 120.0, 58.0, 70.0];
pub(super) const TASK_RATIOS: [f32; 6] = [0.25, 0.26, 0.12, 0.125, 0.135, 0.11];
pub(super) const TASK_MIN: [f32; 6] = [120.0, 150.0, 70.0, 90.0, 90.0, 62.0];

impl<const N: usize> Columns<N> {
    pub(super) fn layout(
        &mut self,
        viewport: f32,
        ratios: [f32; N],
        minimum: [f32; N],
    ) -> Layout<N> {
        let mut widths = *self
            .widths
            .get_or_insert_with(|| std::array::from_fn(|i| (viewport * ratios[i]).max(minimum[i])));
        // Fill a wider viewport without overwriting the user's preferred widths.
        let extra = (viewport - widths.iter().sum::<f32>()).max(0.0);
        widths[N - 1] += extra;
        Layout { widths }
    }

    pub(super) fn header(
        &mut self,
        ui: &mut egui::Ui,
        layout: &mut Layout<N>,
        minimum: [f32; N],
    ) -> egui::Rect {
        let (mut rect, _) =
            ui.allocate_exact_size(Vec2::new(layout.width(), 28.0), egui::Sense::hover());
        for (i, min) in minimum.into_iter().enumerate() {
            let x = layout.cell(rect, i).right() - if i == N - 1 { 2.0 } else { 0.0 };
            let handle = egui::Rect::from_center_size(
                egui::pos2(x, rect.center().y),
                Vec2::new(8.0, rect.height()),
            );
            let response = ui
                .interact(
                    handle,
                    ui.scope_id().with(("column-resize", i)),
                    egui::Sense::drag(),
                )
                .on_hover_cursor(egui::CursorIcon::ResizeHorizontal);
            if response.dragged() {
                layout.widths[i] = (layout.widths[i] + response.drag_delta().x).clamp(min, 4096.0);
                self.widths = Some(layout.widths);
                ui.ctx().request_repaint();
            }
            let x = layout.cell(rect, i).right() - if i == N - 1 { 2.0 } else { 0.0 };
            ui.painter().vline(
                x,
                rect.y_range(),
                egui::Stroke::new(
                    1.0,
                    if response.hovered() || response.dragged() {
                        BLUE
                    } else {
                        BORDER
                    },
                ),
            );
        }
        rect.max.x = rect.left() + layout.width();
        ui.expand_to_include_rect(rect);
        ui.painter().hline(
            rect.x_range(),
            rect.bottom(),
            egui::Stroke::new(1.0, BORDER),
        );
        rect
    }
}

#[derive(Clone, Copy)]
pub(super) struct Layout<const N: usize> {
    widths: [f32; N],
}
impl<const N: usize> Layout<N> {
    pub(super) fn width(&self) -> f32 {
        self.widths.iter().sum()
    }
    pub(super) fn cell(&self, row: egui::Rect, index: usize) -> egui::Rect {
        let left = row.left() + self.widths[..index].iter().sum::<f32>();
        egui::Rect::from_min_size(
            egui::pos2(left, row.top()),
            Vec2::new(self.widths[index], row.height()),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(
        ctx: &egui::Context,
        columns: &mut Columns<4>,
        events: Vec<egui::Event>,
        offset: f32,
    ) -> (egui::Rect, egui::Rect, egui::Rect) {
        let mut geometry = (
            egui::Rect::NOTHING,
            egui::Rect::NOTHING,
            egui::Rect::NOTHING,
        );
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    Vec2::new(600.0, 300.0),
                )),
                events,
                ..Default::default()
            },
            |ui| {
                ui.set_width(400.0);
                ui.spacing_mut().item_spacing = Vec2::ZERO;
                let mut layout = columns.layout(400.0, FILE_RATIOS, FILE_MIN);
                let scroll = egui::ScrollArea::horizontal()
                    .horizontal_scroll_offset(offset)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        ui.set_width(layout.width());
                        let header = columns.header(ui, &mut layout, FILE_MIN);
                        geometry.0 = layout.cell(header, 0);
                        egui::ScrollArea::vertical()
                            .max_height(100.0)
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                let (row, _) = ui.allocate_exact_size(
                                    Vec2::new(layout.width(), 36.0),
                                    egui::Sense::click(),
                                );
                                geometry.1 = layout.cell(row, 0);
                            });
                    });
                geometry.2 = scroll.inner_rect;
            },
        );
        output.textures_delta.clear();
        geometry
    }

    #[test]
    fn header_drag_preserves_other_columns_and_scrolls_header_with_rows() {
        let ctx = egui::Context::default();
        let mut columns = Columns::default();
        let (header, _, _) = frame(&ctx, &mut columns, vec![], 0.0);
        let before = columns.widths.unwrap();
        let start = header.right_center();
        frame(
            &ctx,
            &mut columns,
            vec![egui::Event::PointerMoved(start)],
            0.0,
        );
        frame(
            &ctx,
            &mut columns,
            vec![egui::Event::PointerButton {
                pos: start,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: Default::default(),
            }],
            0.0,
        );
        let end = start + Vec2::new(180.0, 0.0);
        frame(
            &ctx,
            &mut columns,
            vec![egui::Event::PointerMoved(end)],
            0.0,
        );
        frame(
            &ctx,
            &mut columns,
            vec![egui::Event::PointerButton {
                pos: end,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: Default::default(),
            }],
            0.0,
        );
        let after = columns.widths.unwrap();
        assert!(
            (after[0] - before[0] - 180.0).abs() < 1.0,
            "{before:?} -> {after:?}"
        );
        assert_eq!(&after[1..], &before[1..]);
        let (head, row, viewport) = frame(&ctx, &mut columns, vec![], 100.0);
        assert_eq!(head.x_range(), row.x_range());
        assert!(head.left() < viewport.left() - 90.0);
        assert!(columns.layout(400.0, FILE_RATIOS, FILE_MIN).width() > viewport.width());
        let mut other = Columns::default();
        assert_eq!(other.layout(400.0, FILE_RATIOS, FILE_MIN).widths, before);
    }
}

use eframe::egui::{self, CursorIcon, Pos2, Rect, ResizeDirection};

/// Undecorated windows need explicit compositor resize requests on both
/// Wayland and X11. Request on the initial press (Wayland needs its serial).
#[derive(Default)]
pub(super) struct ResizeGrip {
    dragging: bool,
    cursor: Option<CursorIcon>,
}
impl ResizeGrip {
    pub fn intercept(&mut self, ctx: &egui::Context, enabled: bool) -> bool {
        self.cursor = None;
        let (available, pointer, down, pressed) = ctx.input(|i| {
            let viewport = i.viewport();
            (
                enabled
                    && !viewport.fullscreen.unwrap_or(false)
                    && !viewport.maximized.unwrap_or(false),
                i.pointer.hover_pos(),
                i.pointer.primary_down(),
                i.events.iter().find_map(|e| match e {
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        ..
                    } => Some(*pos),
                    _ => None,
                }),
            )
        });
        if !available || !down {
            self.dragging = false;
        }
        if available {
            if let Some(direction) = pressed.and_then(|p| hit_test(ctx.viewport_rect(), p)) {
                self.dragging = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::BeginResize(direction));
                self.cursor = Some(cursor(direction));
            } else if !down {
                self.cursor = pointer
                    .and_then(|p| hit_test(ctx.viewport_rect(), p))
                    .map(cursor);
            }
        }
        let captured = self.dragging || self.cursor.is_some();
        if captured {
            ctx.input_mut(|i| {
                i.events.retain(|e| {
                    !matches!(
                        e,
                        egui::Event::PointerButton { .. }
                            | egui::Event::PointerMoved(_)
                            | egui::Event::MouseMoved(_)
                            | egui::Event::MouseWheel { .. }
                    )
                })
            });
        }
        captured
    }
    pub fn paint_cursor(&self, ctx: &egui::Context) {
        if let Some(cursor) = self.cursor {
            ctx.set_cursor_icon(cursor);
        }
    }
}

fn hit_test(rect: Rect, p: Pos2) -> Option<ResizeDirection> {
    if !rect.contains(p) {
        return None;
    }
    let left = p.x - rect.left() < 6.0;
    let right = rect.right() - p.x < 6.0;
    let top = p.y - rect.top() < 6.0;
    let bottom = rect.bottom() - p.y < 6.0;
    // Wider diagonal grips remain confined to the outside six-pixel strip.
    let near_left = p.x - rect.left() < 14.0;
    let near_right = rect.right() - p.x < 14.0;
    let near_top = p.y - rect.top() < 14.0;
    let near_bottom = rect.bottom() - p.y < 14.0;
    use ResizeDirection::*;
    match (left, right, top, bottom) {
        _ if left && near_top || top && near_left => Some(NorthWest),
        _ if right && near_top || top && near_right => Some(NorthEast),
        _ if left && near_bottom || bottom && near_left => Some(SouthWest),
        _ if right && near_bottom || bottom && near_right => Some(SouthEast),
        (true, _, _, _) => Some(West),
        (_, true, _, _) => Some(East),
        (_, _, true, _) => Some(North),
        (_, _, _, true) => Some(South),
        _ => None,
    }
}
fn cursor(direction: ResizeDirection) -> CursorIcon {
    use ResizeDirection::*;
    match direction {
        North | South => CursorIcon::ResizeVertical,
        East | West => CursorIcon::ResizeHorizontal,
        NorthWest | SouthEast => CursorIcon::ResizeNwSe,
        NorthEast | SouthWest => CursorIcon::ResizeNeSw,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn edges_corners_and_content_are_distinct() {
        let rect = Rect::from_min_max(Pos2::ZERO, egui::pos2(1280.0, 760.0));
        use ResizeDirection::*;
        for (x, y, expected) in [
            (1., 1., NorthWest),
            (1279., 1., NorthEast),
            (1., 759., SouthWest),
            (1279., 759., SouthEast),
            (300., 2., North),
            (300., 759., South),
            (2., 300., West),
            (1279., 300., East),
            (10., 2., NorthWest),
        ] {
            assert_eq!(hit_test(rect, egui::pos2(x, y)), Some(expected));
        }
        assert_eq!(hit_test(rect, egui::pos2(15., 15.)), None);
        assert_eq!(hit_test(rect, egui::pos2(640., 400.)), None);
        assert_eq!(hit_test(rect, egui::pos2(-1., 20.)), None);
    }
    #[test]
    fn native_resize_consumes_edge_press_but_preserves_remote_drag() {
        let ctx = egui::Context::default();
        let mut grip = ResizeGrip::default();
        let rect = Rect::from_min_max(Pos2::ZERO, egui::pos2(1280., 760.));
        let press = |pos| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: Default::default(),
        };
        let input = |events| egui::RawInput {
            screen_rect: Some(rect),
            events,
            ..Default::default()
        };
        let mut output = ctx.run_pass(
            input(vec![
                egui::Event::PointerMoved(egui::pos2(1279., 759.)),
                press(egui::pos2(1279., 759.)),
            ]),
            |ctx| {
                assert!(grip.intercept(ctx, true));
                assert!(ctx.input(|i| i.events.is_empty()));
                grip.paint_cursor(ctx);
            },
        );
        assert!(
            output.viewport_output[&egui::ViewportId::ROOT]
                .commands
                .contains(&egui::ViewportCommand::BeginResize(
                    ResizeDirection::SouthEast
                ))
        );
        assert_eq!(output.platform_output.cursor_icon, CursorIcon::ResizeNwSe);
        output.textures_delta.clear();
        // A button held inside the remote desktop remains remote when crossing the edge.
        let ctx = egui::Context::default();
        let mut grip = ResizeGrip::default();
        let mut initial = ctx.run_pass(input(vec![press(egui::pos2(640., 400.))]), |ctx| {
            assert!(!grip.intercept(ctx, true));
        });
        initial.textures_delta.clear();
        let mut output = ctx.run_pass(
            input(vec![egui::Event::PointerMoved(egui::pos2(1279., 400.))]),
            |ctx| {
                assert!(!grip.intercept(ctx, true));
                assert_eq!(ctx.input(|i| i.events.len()), 1);
            },
        );
        output.textures_delta.clear();
        assert!(
            !output.viewport_output[&egui::ViewportId::ROOT]
                .commands
                .iter()
                .any(|cmd| matches!(cmd, egui::ViewportCommand::BeginResize(_)))
        );
    }
}

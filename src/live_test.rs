//! Explicitly opt-in: sends a Start-button click and Escape to a real Windows
//! device, through the same Rust input mapper and native Rust core as the GUI.
use crate::{
    backend::{Backend, Event as CoreEvent, VideoFrame},
    input::RemoteInput,
};
use eframe::egui::{self, Event, Key, Modifiers, PointerButton, Rect, pos2, vec2};
use serde_json::json;
use std::{
    path::Path,
    thread,
    time::{Duration, Instant},
};

fn frame_after(backend: &Backend, sequence: u64, settle: Duration) -> VideoFrame {
    let started = Instant::now();
    let mut latest = None;
    while started.elapsed() < Duration::from_secs(55) {
        for event in backend.events.try_iter() {
            if let CoreEvent::Error { scope, message, .. } = event {
                panic!("{scope}: {message}");
            }
        }
        if let Some(frame) = backend.latest.lock().unwrap().take()
            && frame.sequence > sequence
        {
            latest = Some(frame);
        }
        if started.elapsed() >= settle
            && let Some(frame) = latest.take()
        {
            return frame;
        }
        thread::sleep(Duration::from_millis(20));
    }
    panic!("no new remote frame");
}

fn save(frame: &VideoFrame, name: &str) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../captures/uux")
        .join(name);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    image::save_buffer_with_format(
        path,
        &frame.rgba,
        frame.width as u32,
        frame.height as u32,
        image::ColorType::Rgba8,
        image::ImageFormat::Png,
    )
    .unwrap();
}

fn send_events(
    backend: &Backend,
    input: &mut RemoteInput,
    events: &[Event],
    rect: Rect,
    dimensions: [usize; 2],
) {
    for command in input.events(events, true, rect, dimensions, None) {
        backend
            .send(json!({"op":"input","generation":1,"command":command}))
            .unwrap();
    }
}

#[test]
#[ignore = "requires UU_TEST_DEVICE; clicks bottom-left Windows Start, then Escape"]
fn live_native_input_and_video() {
    let device = std::env::var("UU_TEST_DEVICE").expect("set an explicitly authorized device ID");
    let mut backend = Backend::start(None, egui::Context::default()).unwrap();
    backend
        .send(json!({"op":"connect","device_id":device,"generation":1}))
        .unwrap();
    let before = frame_after(&backend, 0, Duration::from_secs(2));
    save(&before, "input-before.png");
    let dimensions = [before.width, before.height];
    let rect = Rect::from_min_size(pos2(100.0, 50.0), vec2(800.0, 500.0));
    let position = pos2(
        rect.left() + rect.width() * 0.025,
        rect.top() + rect.height() * 0.98,
    );
    let mut input = RemoteInput::default();
    for pressed in [true, false] {
        send_events(
            &backend,
            &mut input,
            &[Event::PointerButton {
                pos: position,
                button: PointerButton::Primary,
                pressed,
                modifiers: Modifiers::NONE,
            }],
            rect,
            dimensions,
        );
        thread::sleep(Duration::from_millis(50));
    }
    let menu = frame_after(&backend, before.sequence, Duration::from_secs(3));
    save(&menu, "input-start-menu.png");
    for pressed in [true, false] {
        send_events(
            &backend,
            &mut input,
            &[Event::Key {
                key: Key::Escape,
                physical_key: Some(Key::Escape),
                pressed,
                repeat: false,
                modifiers: Modifiers::NONE,
            }],
            rect,
            dimensions,
        );
        thread::sleep(Duration::from_millis(50));
    }
    let after = frame_after(&backend, menu.sequence, Duration::from_secs(3));
    save(&after, "input-escape.png");
    send_events(
        &backend,
        &mut input,
        &[Event::PointerMoved(rect.center())],
        rect,
        dimensions,
    );
    for command in input.release() {
        backend
            .send(json!({"op":"input","generation":1,"command":command}))
            .unwrap();
    }
    backend.shutdown();
    // Visual interpretation is deliberately separate: desktop clocks and other
    // animations make a generic "pixels changed" assertion misleading.
    eprintln!(
        "Received {}×{} frames {}; {}; {}. Inspect ../captures/uux/input-*.png.",
        before.width, before.height, before.sequence, menu.sequence, after.sequence
    );
}

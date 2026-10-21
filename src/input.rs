use eframe::egui::{Event, Key, Modifiers, MouseWheelUnit, PointerButton, Pos2, Rect};
use serde_json::{Value, json};
use std::collections::BTreeSet;

#[derive(Default)]
pub struct RemoteInput {
    pub armed: bool,
    pub local_clipboard: bool,
    pub release_shortcut: Option<String>,
    composing: bool,
    keys: BTreeSet<u16>,
    buttons: BTreeSet<u8>,
    wheel: [f32; 2],
    hover_inside: bool,
}

pub fn shortcut_from_event(event: &Event) -> Option<String> {
    let Event::Key {
        key,
        pressed: true,
        modifiers,
        ..
    } = event
    else {
        return None;
    };
    if matches!(key, Key::SuperLeft | Key::SuperRight)
        || !(modifiers.ctrl || modifiers.alt || modifiers.shift || modifiers.mac_cmd)
    {
        return None;
    }
    let mut parts = Vec::new();
    if modifiers.ctrl {
        parts.push("Ctrl");
    }
    if modifiers.alt {
        parts.push("Alt");
    }
    if modifiers.shift {
        parts.push("Shift");
    }
    if modifiers.mac_cmd {
        parts.push("Super");
    }
    parts.push(key.name());
    Some(parts.join("+"))
}
pub fn shortcut_matches(spec: &str, event: &Event) -> bool {
    shortcut_from_event(event).is_some_and(|s| s.eq_ignore_ascii_case(spec))
}

pub fn coordinates(rect: Rect, pos: Pos2, width: usize, height: usize) -> Option<(f32, f32)> {
    if rect.width() <= 0.0 || rect.height() <= 0.0 || width == 0 || height == 0 {
        return None;
    }
    Some((
        ((pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0 - 1.0 / width as f32),
        ((pos.y - rect.top()) / rect.height()).clamp(0.0, 1.0 - 1.0 / height as f32),
    ))
}

pub fn virtual_key(key: Key) -> Option<u16> {
    if (Key::A..=Key::Z).contains(&key) {
        return Some(0x41 + key as u16 - Key::A as u16);
    }
    if (Key::Num0..=Key::Num9).contains(&key) {
        return Some(0x30 + key as u16 - Key::Num0 as u16);
    }
    if (Key::F1..=Key::F24).contains(&key) {
        return Some(0x70 + key as u16 - Key::F1 as u16);
    }
    Some(match key {
        Key::Backspace => 8,
        Key::Tab => 9,
        Key::Enter => 13,
        Key::Escape => 27,
        Key::Space => 32,
        Key::CapsLock => 20,
        Key::NumLock => 144,
        Key::ScrollLock => 145,
        Key::PrintScreen => 44,
        Key::Pause => 19,
        Key::Menu => 93,
        Key::SuperLeft => 91,
        Key::SuperRight => 92,
        Key::PageUp => 33,
        Key::PageDown => 34,
        Key::End => 35,
        Key::Home => 36,
        Key::ArrowLeft => 37,
        Key::ArrowUp => 38,
        Key::ArrowRight => 39,
        Key::ArrowDown => 40,
        Key::Insert => 45,
        Key::Delete => 46,
        Key::Semicolon | Key::Colon => 0xba,
        Key::Equals | Key::Plus => 0xbb,
        Key::Comma => 0xbc,
        Key::Minus => 0xbd,
        Key::Period => 0xbe,
        Key::Slash | Key::Questionmark => 0xbf,
        Key::Backtick => 0xc0,
        Key::OpenBracket | Key::OpenCurlyBracket => 0xdb,
        Key::Backslash | Key::Pipe => 0xdc,
        Key::CloseBracket | Key::CloseCurlyBracket => 0xdd,
        Key::Quote => 0xde,
        _ => return None,
    })
}

fn button_number(button: PointerButton) -> u8 {
    match button {
        PointerButton::Primary => 1,
        PointerButton::Secondary => 2,
        PointerButton::Middle => 16,
        PointerButton::Extra1 => 32,
        PointerButton::Extra2 => 64,
    }
}

impl RemoteInput {
    /// Focus loss / local overlays reset the entry edge. Explicit release keeps
    /// the edge, so the escape shortcut stays effective until leave and re-enter.
    pub fn suspend(&mut self) -> Vec<Value> {
        self.hover_inside = false;
        self.release()
    }
    fn hover(&mut self, inside: bool) -> Vec<Value> {
        if inside {
            if !self.hover_inside {
                self.armed = true;
            }
            self.hover_inside = true;
            vec![]
        } else {
            self.hover_inside = false;
            if self.buttons.is_empty() {
                self.release()
            } else {
                vec![]
            }
        }
    }
    pub fn release(&mut self) -> Vec<Value> {
        let send = self.armed || !self.keys.is_empty() || !self.buttons.is_empty();
        self.armed = false;
        self.composing = false;
        self.keys.clear();
        self.buttons.clear();
        self.wheel = [0.0; 2];
        if send {
            vec![json!({"op":"release"})]
        } else {
            vec![]
        }
    }

    fn modifiers(&mut self, mods: Modifiers, out: &mut Vec<Value>) {
        for (key, pressed) in [(16, mods.shift), (17, mods.ctrl), (18, mods.alt)] {
            if pressed && self.keys.insert(key) {
                out.push(json!({"op":"down","key":key}));
            } else if !pressed && self.keys.remove(&key) {
                out.push(json!({"op":"up","key":key}));
            }
        }
    }

    pub fn events(
        &mut self,
        events: &[Event],
        focused: bool,
        rect: Rect,
        size: [usize; 2],
        pointer: Option<Pos2>,
    ) -> Vec<Value> {
        if !focused {
            return self.suspend();
        }
        let mut out = Vec::new();
        // Pointer location is also available on frames with only keyboard input.
        if let Some(pos) = pointer
            && !events.iter().any(|e| matches!(e, Event::PointerMoved(_)))
        {
            let was_armed = self.armed;
            out.extend(self.hover(rect.contains(pos)));
            if !was_armed
                && self.armed
                && let Some((x, y)) = coordinates(rect, pos, size[0], size[1])
            {
                out.push(json!({"op":"move","x":x,"y":y}));
            }
        }
        // Winit can deliver the commit and its Enter key in either order in
        // the same frame. Do not send candidate-selection keystrokes remotely.
        let ime_batch = events.iter().any(|e| matches!(e,
            Event::Ime(eframe::egui::ImeEvent::Commit(_)))
            || matches!(e, Event::Ime(eframe::egui::ImeEvent::Preedit {text,..}) if !text.is_empty()));
        if self.armed && ime_batch {
            for key in std::mem::take(&mut self.keys) {
                out.push(json!({"op":"up","key":key}));
            }
        }
        for event in events {
            if shortcut_matches(
                self.release_shortcut.as_deref().unwrap_or("Ctrl+Alt+G"),
                event,
            ) {
                out.extend(self.release());
                continue;
            }
            match event {
                Event::Ime(eframe::egui::ImeEvent::Preedit { text, .. }) if self.armed => {
                    self.composing = !text.is_empty();
                }
                Event::Ime(eframe::egui::ImeEvent::Commit(text)) if self.armed => {
                    self.composing = false;
                    if !text.is_empty() {
                        out.push(json!({"op":"text","text":text}));
                    }
                }
                Event::Text(text)
                    if self.armed
                        && !text.is_ascii()
                        && !events.iter().any(|e| {
                            matches!(e, Event::Ime(eframe::egui::ImeEvent::Commit(_)))
                        }) =>
                {
                    out.push(json!({"op":"text","text":text}));
                }
                Event::Paste(text) if self.armed => out.push(json!({"op":"text","text":text})),
                Event::WindowFocused(false) | Event::PointerGone => out.extend(self.suspend()),
                Event::Key {
                    key: Key::V,
                    pressed: true,
                    modifiers,
                    ..
                } if self.armed
                    && self.local_clipboard
                    && modifiers.ctrl
                    && !modifiers.alt
                    && !self.composing
                    && !ime_batch =>
                {
                    for key in std::mem::take(&mut self.keys) {
                        out.push(json!({"op":"up","key":key}));
                    }
                    out.push(json!({"op":"paste_local"}));
                }
                Event::PointerButton {
                    pos,
                    button,
                    pressed,
                    modifiers,
                } => {
                    let number = button_number(*button);
                    if *pressed && rect.contains(*pos) {
                        self.hover_inside = true;
                        self.armed = true;
                        self.modifiers(*modifiers, &mut out);
                        if let Some((x, y)) = coordinates(rect, *pos, size[0], size[1]) {
                            out.push(json!({"op":"move","x":x,"y":y}));
                        }
                        self.buttons.insert(number);
                        out.push(json!({"op":"button_down","button":number}));
                    } else if *pressed {
                        out.extend(self.release());
                    } else if self.buttons.remove(&number) {
                        out.push(json!({"op":"button_up","button":number}));
                        if !rect.contains(*pos) {
                            out.extend(self.hover(false));
                        }
                    }
                }
                Event::PointerMoved(pos) => {
                    out.extend(self.hover(rect.contains(*pos)));
                    if self.armed
                        && (rect.contains(*pos) || !self.buttons.is_empty())
                        && let Some((x, y)) = coordinates(rect, *pos, size[0], size[1])
                    {
                        let movement = json!({"op":"move","x":x,"y":y});
                        if out.last().is_some_and(|v| v["op"] == "move") {
                            *out.last_mut().unwrap() = movement;
                        } else {
                            out.push(movement);
                        }
                    }
                }
                Event::ModifiersChanged(mods) if self.armed && !self.composing && !ime_batch => {
                    self.modifiers(*mods, &mut out)
                }
                Event::Key {
                    key,
                    physical_key,
                    pressed,
                    repeat: _,
                    modifiers,
                } if self.armed && !self.composing && !ime_batch => {
                    self.modifiers(*modifiers, &mut out);
                    if let Some(vk) = virtual_key(physical_key.unwrap_or(*key)) {
                        if *pressed {
                            // Each native event is consumed once, including OS
                            // repeats. Egui hasn't annotated `repeat` yet.
                            self.keys.insert(vk);
                            out.push(json!({"op":"down","key":vk}));
                        } else if self.keys.remove(&vk) {
                            out.push(json!({"op":"up","key":vk}));
                        }
                    }
                }
                Event::Key {
                    key,
                    physical_key,
                    pressed: false,
                    ..
                } if self.armed => {
                    if let Some(vk) = virtual_key(physical_key.unwrap_or(*key))
                        && self.keys.remove(&vk)
                    {
                        out.push(json!({"op":"up","key":vk}));
                    }
                }
                Event::MouseWheel { delta, unit, .. }
                    if self.armed && pointer.is_some_and(|p| rect.contains(p)) =>
                {
                    let scale = match unit {
                        MouseWheelUnit::Point => 3.0,
                        MouseWheelUnit::Line => 120.0,
                        MouseWheelUnit::Page => 600.0,
                    };
                    self.wheel[0] -= delta.x * scale;
                    self.wheel[1] += delta.y * scale;
                    let dx = self.wheel[0].trunc() as i32;
                    let dy = self.wheel[1].trunc() as i32;
                    self.wheel[0] -= dx as f32;
                    self.wheel[1] -= dy as f32;
                    if dx != 0 || dy != 0 {
                        out.push(json!({"op":"scroll","dx":dx,"dy":dy}));
                    }
                }
                _ => {}
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::{pos2, vec2};

    fn rect() -> Rect {
        Rect::from_min_size(pos2(100.0, 50.0), vec2(800.0, 500.0))
    }
    fn key(key: Key, pressed: bool, modifiers: Modifiers) -> Event {
        Event::Key {
            key,
            physical_key: Some(key),
            pressed,
            repeat: false,
            modifiers,
        }
    }

    #[test]
    fn scaled_coordinates_exclude_letterbox_and_clamp_edge() {
        assert_eq!(
            coordinates(rect(), pos2(500.0, 300.0), 1680, 1050),
            Some((0.5, 0.5))
        );
        let edge = coordinates(rect(), pos2(999.0, 999.0), 1680, 1050).unwrap();
        assert!(edge.0 < 1.0 && edge.1 < 1.0);
        let mut input = RemoteInput::default();
        let out = input.events(
            &[Event::PointerButton {
                pos: pos2(50.0, 200.0),
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            }],
            true,
            rect(),
            [1680, 1050],
            None,
        );
        assert!(out.is_empty() && !input.armed);
    }

    #[test]
    fn focus_loss_releases_keyboard_and_drag_outside_releases_button() {
        let mut input = RemoteInput {
            armed: true,
            ..Default::default()
        };
        assert_eq!(
            input.events(
                &[key(Key::A, true, Modifiers::CTRL)],
                true,
                rect(),
                [1680, 1050],
                None
            ),
            vec![json!({"op":"down","key":17}), json!({"op":"down","key":65})]
        );
        assert_eq!(
            input.events(&[], false, rect(), [1680, 1050], None),
            vec![json!({"op":"release"})]
        );
        assert!(!input.armed);
        let down = Event::PointerButton {
            pos: pos2(200.0, 200.0),
            button: PointerButton::Primary,
            pressed: true,
            modifiers: Modifiers::NONE,
        };
        input.events(&[down], true, rect(), [1680, 1050], None);
        let up = Event::PointerButton {
            pos: pos2(50.0, 10.0),
            button: PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        };
        assert_eq!(
            input.events(&[up], true, rect(), [1680, 1050], None),
            vec![
                json!({"op":"button_up","button":1}),
                json!({"op":"release"})
            ]
        );
    }

    #[test]
    fn hover_arms_without_click_and_manual_release_waits_for_reentry() {
        let inside = pos2(200.0, 200.0);
        let outside = pos2(20.0, 20.0);
        let mut input = RemoteInput::default();
        let out = input.events(
            &[
                Event::PointerMoved(inside),
                key(Key::A, true, Modifiers::NONE),
            ],
            true,
            rect(),
            [1680, 1050],
            Some(inside),
        );
        assert!(input.armed);
        assert_eq!(out[0]["op"], "move");
        assert_eq!(out[1], json!({"op":"down","key":65}));
        let mods = Modifiers {
            ctrl: true,
            alt: true,
            ..Default::default()
        };
        assert_eq!(
            input.events(
                &[key(Key::G, true, mods)],
                true,
                rect(),
                [1680, 1050],
                Some(inside)
            ),
            vec![json!({"op":"release"})]
        );
        assert!(
            input
                .events(
                    &[
                        Event::PointerMoved(inside),
                        key(Key::B, true, Modifiers::NONE)
                    ],
                    true,
                    rect(),
                    [1680, 1050],
                    Some(inside)
                )
                .is_empty()
        );
        assert!(!input.armed);
        input.events(
            &[Event::PointerMoved(outside)],
            true,
            rect(),
            [1680, 1050],
            Some(outside),
        );
        input.events(
            &[Event::PointerMoved(inside)],
            true,
            rect(),
            [1680, 1050],
            Some(inside),
        );
        assert!(input.armed);
        assert_eq!(
            input.events(&[Event::PointerGone], true, rect(), [1680, 1050], None),
            vec![json!({"op":"release"})]
        );
        assert!(!input.armed);
    }

    #[test]
    fn hover_outside_releases_keys_but_finishes_drag_before_releasing() {
        let mut input = RemoteInput::default();
        input.events(
            &[
                Event::PointerMoved(pos2(200.0, 200.0)),
                key(Key::A, true, Modifiers::CTRL),
            ],
            true,
            rect(),
            [1680, 1050],
            None,
        );
        assert_eq!(
            input.events(
                &[Event::PointerMoved(pos2(20.0, 20.0))],
                true,
                rect(),
                [1680, 1050],
                None
            ),
            vec![json!({"op":"release"})]
        );
        input.events(
            &[Event::PointerButton {
                pos: pos2(200.0, 200.0),
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            }],
            true,
            rect(),
            [1680, 1050],
            None,
        );
        let out = input.events(
            &[Event::PointerMoved(pos2(20.0, 20.0))],
            true,
            rect(),
            [1680, 1050],
            None,
        );
        assert!(input.armed);
        assert_eq!(out[0]["op"], "move");
        assert!(input.buttons.contains(&1));
        let out = input.events(&[], false, rect(), [1680, 1050], None);
        assert_eq!(out, vec![json!({"op":"release"})]);
    }

    #[test]
    fn escape_is_remote_but_release_shortcut_is_local() {
        let mut input = RemoteInput {
            armed: true,
            ..Default::default()
        };
        let out = input.events(
            &[
                key(Key::Escape, true, Modifiers::NONE),
                key(Key::Escape, false, Modifiers::NONE),
            ],
            true,
            rect(),
            [1680, 1050],
            None,
        );
        assert_eq!(
            out,
            vec![json!({"op":"down","key":27}), json!({"op":"up","key":27})]
        );
        let mods = Modifiers {
            ctrl: true,
            alt: true,
            ..Default::default()
        };
        assert_eq!(
            input.events(&[key(Key::G, true, mods)], true, rect(), [1680, 1050], None),
            vec![json!({"op":"release"})]
        );
        assert_eq!(virtual_key(Key::F12), Some(123));
        assert_eq!(virtual_key(Key::Z), Some(90));
    }

    #[test]
    fn custom_release_shortcut_replaces_default_and_releases_held_keys() {
        let mut input = RemoteInput {
            armed: true,
            release_shortcut: Some("Ctrl+Shift+R".into()),
            ..Default::default()
        };
        let old_mods = Modifiers {
            ctrl: true,
            alt: true,
            ..Default::default()
        };
        let old = input.events(
            &[key(Key::G, true, old_mods)],
            true,
            rect(),
            [1680, 1050],
            None,
        );
        assert!(input.armed);
        assert!(old.contains(&json!({"op":"down","key":71})));
        let mods = Modifiers {
            ctrl: true,
            shift: true,
            ..Default::default()
        };
        let event = key(Key::R, true, mods);
        assert!(shortcut_matches("ctrl+shift+r", &event));
        assert!(!shortcut_matches("Ctrl+R", &event));
        assert_eq!(
            input.events(&[event], true, rect(), [1680, 1050], None),
            vec![json!({"op":"release"})]
        );
        assert!(!input.armed);
        assert!(input.keys.is_empty());
    }

    #[test]
    fn raw_repeats_and_super_chord_keep_matching_releases() {
        let mut input = RemoteInput {
            armed: true,
            ..Default::default()
        };
        let events = [
            key(Key::SuperLeft, true, Modifiers::NONE),
            key(Key::A, true, Modifiers::NONE),
            key(Key::A, true, Modifiers::NONE), // Native repeat before egui annotates it.
            key(Key::A, false, Modifiers::NONE),
            key(Key::SuperLeft, false, Modifiers::NONE),
        ];
        let output = input.events(&events, true, rect(), [1680, 1050], None);
        assert_eq!(
            output,
            vec![
                json!({"op":"down","key":91}),
                json!({"op":"down","key":65}),
                json!({"op":"down","key":65}),
                json!({"op":"up","key":65}),
                json!({"op":"up","key":91}),
            ]
        );
        assert!(input.keys.is_empty());
    }
    #[test]
    fn ime_commit_does_not_duplicate_text_or_send_candidate_enter() {
        let mut input = RemoteInput {
            armed: true,
            ..Default::default()
        };
        let events = [
            key(Key::Enter, true, Modifiers::NONE),
            Event::Ime(eframe::egui::ImeEvent::Commit("你好 Rust".into())),
            Event::Text("你好 Rust".into()),
            key(Key::Enter, false, Modifiers::NONE),
        ];
        assert_eq!(
            input.events(&events, true, rect(), [1680, 1050], None),
            vec![json!({"op":"text","text":"你好 Rust"})]
        );
    }
    #[test]
    fn starting_composition_releases_previously_pressed_shortcut() {
        let mut input = RemoteInput {
            armed: true,
            ..Default::default()
        };
        input.events(
            &[key(Key::A, true, Modifiers::CTRL)],
            true,
            rect(),
            [1680, 1050],
            None,
        );
        let events = [Event::Ime(eframe::egui::ImeEvent::Commit("中".into()))];
        let out = input.events(&events, true, rect(), [1680, 1050], None);
        assert_eq!(
            out,
            vec![
                json!({"op":"up","key":17}),
                json!({"op":"up","key":65}),
                json!({"op":"text","text":"中"})
            ]
        );
        assert!(input.keys.is_empty());
    }
}

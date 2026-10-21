use super::*;
use base64::Engine;
use std::sync::Arc;
#[derive(Clone)]
pub struct Shot {
    pub owner: String,
    pub session: String,
    pub source: [usize; 2],
    pub crop: [u32; 4],
    pub image: [u32; 2],
    pub screen: Value,
    pub created: Instant,
}
impl Shot {
    pub fn point(&self, x: f64, y: f64) -> Result<(f64, f64)> {
        ensure!(
            x.is_finite()
                && y.is_finite()
                && x >= 0.0
                && y >= 0.0
                && x < f64::from(self.image[0])
                && y < f64::from(self.image[1]),
            "坐标不在截图范围内"
        );
        Ok((
            (f64::from(self.crop[0]) + x * f64::from(self.crop[2]) / f64::from(self.image[0]))
                / self.source[0] as f64,
            (f64::from(self.crop[1]) + y * f64::from(self.crop[3]) / f64::from(self.image[1]))
                / self.source[1] as f64,
        ))
    }
}
fn screen_signature(s: &crate::service::Managed) -> Value {
    let data = s.snapshot.get("screens").cloned().unwrap_or(Value::Null);
    let screen = data["screens"]
        .as_array()
        .and_then(|v| v.iter().find(|r| r["id"] == data["current"]));
    json!({"id":data["current"],"width":screen.map(|v|&v["width"]),"height":screen.map(|v|&v["height"])})
}
pub fn call(hub: &Shared, client: &str, name: &str, args: &Value) -> Result<Value> {
    let id = sid(args)?;
    match name {
        "uu_screens_list" => {
            let h = hub.lock().unwrap();
            let s = h.ready(client, id)?;
            Ok(
                json!({"screens":s.snapshot.get("screens"),"super_screen":s.snapshot.get("super_screen"),"capture":s.snapshot.get("remote_capture")}),
            )
        }
        "uu_screenshot" => {
            hub.lock().unwrap().video(client, id, true)?;
            let wait = args["wait_ms"].as_u64().unwrap_or(5000).min(30000);
            let start = Instant::now();
            let (frame, screen) = loop {
                {
                    let h = hub.lock().unwrap();
                    let s = h.ready(client, id)?;
                    if let Some(frame) = &s.frame {
                        break (Arc::clone(frame), screen_signature(s));
                    }
                }
                ensure!(
                    start.elapsed() < Duration::from_millis(wait),
                    "等待画面超时"
                );
                std::thread::sleep(Duration::from_millis(20));
            };
            let (w, h) = (frame.width as u32, frame.height as u32);
            let crop = if args["crop"].is_object() {
                let c = &args["crop"];
                [
                    u32::try_from(c["x"].as_u64().context("crop.x 无效")?)?,
                    u32::try_from(c["y"].as_u64().context("crop.y 无效")?)?,
                    u32::try_from(c["width"].as_u64().context("crop.width 无效")?)?,
                    u32::try_from(c["height"].as_u64().context("crop.height 无效")?)?,
                ]
            } else {
                [0, 0, w, h]
            };
            ensure!(
                crop[2] > 0
                    && crop[3] > 0
                    && crop[0].checked_add(crop[2]).is_some_and(|x| x <= w)
                    && crop[1].checked_add(crop[3]).is_some_and(|y| y <= h),
                "裁剪区域不在远程画面内"
            );
            let max = args["max_width"].as_u64().unwrap_or(1600);
            ensure!((64..=3840).contains(&max), "截图宽度须在 64–3840 之间");
            let rgba =
                image::RgbaImage::from_raw(w, h, frame.rgba.clone()).context("帧长度无效")?;
            let cropped =
                image::imageops::crop_imm(&rgba, crop[0], crop[1], crop[2], crop[3]).to_image();
            let image = image::DynamicImage::ImageRgba8(cropped).resize(
                max as u32,
                2160,
                image::imageops::FilterType::Triangle,
            );
            let dims = [image.width(), image.height()];
            let mut bytes = std::io::Cursor::new(Vec::new());
            image.write_to(&mut bytes, image::ImageFormat::Png)?;
            let frame_id = uid();
            let mut h = hub.lock().unwrap();
            h.shots
                .retain(|_, s| s.created.elapsed() < Duration::from_secs(120));
            ensure!(h.shots.len() < 128, "截图引用过多，请稍后重试");
            h.shots.insert(
                frame_id.clone(),
                Shot {
                    owner: client.into(),
                    session: id.into(),
                    source: [w as usize, frame.height],
                    crop,
                    image: dims,
                    screen: screen.clone(),
                    created: Instant::now(),
                },
            );
            Ok(
                json!({"frame_id":frame_id,"image_base64":base64::engine::general_purpose::STANDARD.encode(bytes.into_inner()),"mime_type":"image/png","image_width":dims[0],"image_height":dims[1],"source_width":w,"source_height":frame.height,"crop":crop,"screen":screen,"frame_age_ms":frame.decoded_at.elapsed().as_millis() as u64,"sequence":frame.sequence}),
            )
        }
        "uu_desktop_action" | "uu_type_text" => {
            let _operation = Operation::acquire(hub, id, "input")?;
            let action = if name == "uu_type_text" {
                "text"
            } else {
                str_arg(args, "action")?
            };
            let point_needed = matches!(
                action,
                "move" | "click" | "double_click" | "right_click" | "drag"
            );
            let shot = {
                let mut h = hub.lock().unwrap();
                let s = h.ready(client, id)?;
                ensure!(
                    s.input_owner.as_deref().is_none_or(|owner| owner == client)
                        || s.input_until <= Instant::now(),
                    "键鼠当前由 GUI 或其他客户端使用"
                );
                let shot = if point_needed {
                    let shot = h
                        .shots
                        .get(str_arg(args, "frame_id")?)
                        .context("截图引用已过期")?
                        .clone();
                    ensure!(
                        shot.owner == client
                            && shot.session == id
                            && shot.created.elapsed() < Duration::from_secs(120),
                        "截图属于其他客户端或会话"
                    );
                    ensure!(
                        shot.screen == screen_signature(s)
                            && s.frame
                                .as_ref()
                                .is_some_and(|f| [f.width, f.height] == shot.source),
                        "屏幕尺寸或显示器已改变，请重新截图"
                    );
                    Some(shot)
                } else {
                    None
                };
                let s = h.sessions.get_mut(id).unwrap();
                if s.input_owner
                    .as_deref()
                    .is_some_and(|owner| owner != client)
                {
                    s.send(json!({"op":"release"}))?;
                }
                s.input_owner = Some(client.into());
                s.input_until = Instant::now() + Duration::from_secs(3);
                shot
            };
            let mut commands = vec![];
            let button = args["button"]
                .as_u64()
                .unwrap_or(if action == "right_click" { 2 } else { 1 });
            if let Some(shot) = &shot {
                let (x, y) = shot.point(
                    args["x"].as_f64().context("x 无效")?,
                    args["y"].as_f64().context("y 无效")?,
                )?;
                commands.push(json!({"op":"move","x":x,"y":y}));
            }
            match action {
                "move" => {}
                "click" | "right_click" | "double_click" => {
                    commands.push(json!({"op":"click","button":button}));
                    if action == "double_click" {
                        commands.push(json!({"op":"click","button":button}));
                    }
                }
                "drag" => {
                    let shot = shot.as_ref().unwrap();
                    let (x, y) = shot.point(
                        args["to_x"].as_f64().context("to_x 无效")?,
                        args["to_y"].as_f64().context("to_y 无效")?,
                    )?;
                    commands.extend([
                        json!({"op":"button_down","button":button}),
                        json!({"op":"move","x":x,"y":y}),
                        json!({"op":"button_up","button":button}),
                    ]);
                }
                "scroll" => {
                    let dx = args["dx"].as_i64().unwrap_or(0);
                    let dy = args["dy"].as_i64().unwrap_or(0);
                    ensure!(
                        dx.unsigned_abs() <= 12000 && dy.unsigned_abs() <= 12000,
                        "滚轮增量过大"
                    );
                    commands.push(json!({"op":"scroll","dx":dx,"dy":dy}));
                }
                "key" | "key_down" | "key_up" => {
                    let keys = args["keys"].as_array().context("需要 keys 数组")?;
                    ensure!(!keys.is_empty() && keys.len() <= 16, "按键数量无效");
                    let keys = keys.iter().map(keycode).collect::<Result<Vec<_>>>()?;
                    if action != "key_up" {
                        for key in &keys {
                            commands.push(json!({"op":"down","key":key}));
                        }
                    }
                    if action != "key_down" {
                        for key in keys.iter().rev() {
                            commands.push(json!({"op":"up","key":key}));
                        }
                    }
                }
                "text" => commands.push(json!({"op":"text","text":str_arg(args,"text")?})),
                "release" => commands.push(json!({"op":"release"})),
                _ => bail!("未知键鼠操作"),
            }
            for command in commands {
                if let Err(error) = submit(hub, client, id, command) {
                    let _ = session_send(hub, client, id, json!({"op":"release"}));
                    return Err(error);
                }
                if action == "drag" || action == "double_click" {
                    std::thread::sleep(Duration::from_millis(45));
                }
            }
            Ok(json!({"submitted":true,"visual_confirmation_required":true}))
        }
        "uu_display_configure" => {
            let a = args.clone();
            start_job(hub, client, id, "display", move |h, c, s, j| {
                let _operation = Operation::acquire(&h, &s, "display")?;
                let cursor = h.lock().unwrap().ready(&c, &s)?.events.seq;
                if let Some(enabled) = a["super_screen"].as_bool() {
                    submit(&h, &c, &s, json!({"op":"super_screen","enabled":enabled}))?;
                    let event = wait_event(&h, &c, &s, cursor, Duration::from_secs(25), |v| {
                        v["kind"] == "super_screen"
                            && matches!(
                                v["data"]["phase"].as_str(),
                                Some("ready" | "failed" | "timeout")
                            )
                    })?;
                    ensure!(
                        event["data"]["phase"] == "ready" && event["data"]["active"] == enabled,
                        "超级屏切换失败"
                    );
                }
                check_cancel(&h, &j)?;
                if ["width", "height", "fps", "quality", "screen_id", "bitrate"]
                    .iter()
                    .any(|k| a.get(k).is_some())
                {
                    let current = h
                        .lock()
                        .unwrap()
                        .ready(&c, &s)?
                        .snapshot
                        .get("screens")
                        .cloned()
                        .unwrap_or(Value::Null);
                    let mut command = a.clone();
                    command["op"] = json!("capture");
                    command["ui_request"] = json!(j);
                    if command["screen_id"].is_null() {
                        command["screen_id"] = current["current"].clone();
                    }
                    submit(&h, &c, &s, command)?;
                    let submitted = wait_event(&h, &c, &s, cursor, Duration::from_secs(5), |v| {
                        v["kind"] == "capture_submitted" && v["data"]["ui_request"] == j
                    })?;
                    let result = wait_event(&h, &c, &s, cursor, Duration::from_secs(40), |v| {
                        v["kind"] == "capture_result"
                            && v["data"]["request_id"] == submitted["data"]["request_id"]
                    })?;
                    ensure!(
                        result["data"]["errors"]
                            .as_array()
                            .is_some_and(|errors| errors.iter().all(|e| e["code"] == 0)),
                        "远端拒绝远控参数"
                    );
                    if let (Some(w), Some(height)) = (a["width"].as_i64(), a["height"].as_i64()) {
                        let start = Instant::now();
                        loop {
                            check_cancel(&h, &j)?;
                            let sig = screen_signature(h.lock().unwrap().ready(&c, &s)?);
                            if sig["width"] == w && sig["height"] == height {
                                break;
                            }
                            ensure!(
                                start.elapsed() < Duration::from_secs(40),
                                "远端已接收设置，但桌面分辨率尚未生效"
                            );
                            std::thread::sleep(Duration::from_millis(50));
                        }
                    }
                }
                let h = h.lock().unwrap();
                let s = h.ready(&c, &s)?;
                Ok(
                    json!({"screens":s.snapshot.get("screens"),"super_screen":s.snapshot.get("super_screen")}),
                )
            })
        }
        "uu_clipboard_get" | "uu_clipboard_set" => {
            let _operation = Operation::acquire(hub, id, "clipboard")?;
            let cursor = hub.lock().unwrap().ready(client, id)?.events.seq;
            let get = name == "uu_clipboard_get";
            submit(
                hub,
                client,
                id,
                if get {
                    json!({"op":"clipboard_get"})
                } else {
                    json!({"op":"clipboard","text":args["text"].as_str().context("缺少 text")?})
                },
            )?;
            let event = wait_event(hub, client, id, cursor, Duration::from_secs(10), |v| {
                v["kind"]
                    == if get {
                        "clipboard_value"
                    } else {
                        "clipboard_sent"
                    }
            })?;
            if !get {
                ensure!(event["data"]["error"] == 1, "远端未确认剪贴板写入");
            }
            Ok(if get {
                event["data"].clone()
            } else {
                json!({"written":true,"source":"remote","confirmed_at_ms":now_ms()})
            })
        }
        _ => bail!("未知桌面操作"),
    }
}
fn keycode(v: &Value) -> Result<u16> {
    if let Some(n) = v.as_u64() {
        ensure!((1..=255).contains(&n), "键码无效");
        return Ok(n as u16);
    }
    let key = v
        .as_str()
        .context("按键必须为名称或 Windows VK 编号")?
        .to_uppercase();
    let n = match key.as_str() {
        "CTRL" | "CONTROL" => 17,
        "SHIFT" => 16,
        "ALT" => 18,
        "WIN" | "SUPER" => 91,
        "ENTER" | "RETURN" => 13,
        "ESC" | "ESCAPE" => 27,
        "TAB" => 9,
        "BACKSPACE" => 8,
        "DELETE" => 46,
        "SPACE" => 32,
        "UP" => 38,
        "DOWN" => 40,
        "LEFT" => 37,
        "RIGHT" => 39,
        "HOME" => 36,
        "END" => 35,
        "PAGEUP" => 33,
        "PAGEDOWN" => 34,
        _ if key.len() == 1 && key.as_bytes()[0].is_ascii_alphanumeric() => {
            u16::from(key.as_bytes()[0])
        }
        _ if key.starts_with('F') => {
            let n = key[1..].parse::<u16>()?;
            ensure!((1..=24).contains(&n), "功能键无效");
            111 + n
        }
        _ => bail!("不支持的按键名称 {key}"),
    };
    Ok(n)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cropped_image_coordinates_map_to_full_frame() {
        let s = Shot {
            owner: String::new(),
            session: String::new(),
            source: [1920, 1080],
            crop: [480, 270, 960, 540],
            image: [480, 270],
            screen: Value::Null,
            created: Instant::now(),
        };
        assert_eq!(s.point(240., 135.).unwrap(), (0.5, 0.5));
        assert!(s.point(480., 0.).is_err());
        assert!(s.point(f64::NAN, 0.).is_err());
    }
    #[test]
    fn named_keys_and_numeric_keys_agree() {
        assert_eq!(keycode(&json!("Ctrl")).unwrap(), 17);
        assert_eq!(keycode(&json!("F24")).unwrap(), 135);
        assert!(keycode(&json!("F25")).is_err());
    }
}

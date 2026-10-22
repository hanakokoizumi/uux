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
    let geometry = s.backend.media.geometry();
    let remote = s
        .snapshot
        .get("screens")
        .and_then(|v| v["screens"].as_array())
        .and_then(|rows| rows.iter().find(|r| r["id"] == geometry.screen_id));
    json!({"id":geometry.screen_id,"width":geometry.width,"height":geometry.height,"version":geometry.version,"remote":remote})
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
                    if let Some(frame) = &s.frame
                        && frame.geometry == s.backend.media.geometry()
                    {
                        if let Some(screen) = args["screen_id"].as_i64() {
                            ensure!(
                                screen == i64::from(frame.geometry.screen_id),
                                "当前接收画面不属于请求的屏幕，请先配置屏幕"
                            );
                        }
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
                json!({"frame_id":frame_id,"image_base64":base64::engine::general_purpose::STANDARD.encode(bytes.into_inner()),"mime_type":"image/png","image_width":dims[0],"image_height":dims[1],"source_width":w,"source_height":frame.height,"crop":crop,"screen":screen,"geometry":frame.geometry,"frame_age_ms":frame.decoded_at.elapsed().as_millis() as u64,"sequence":frame.sequence}),
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
                "move"
                    | "click"
                    | "double_click"
                    | "right_click"
                    | "drag"
                    | "button_down"
                    | "button_up"
                    | "relative"
                    | "scroll"
                    | "touch"
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
                    let shot = if let Some(binding) = args.get("frame") {
                        let geometry = s.backend.media.geometry();
                        ensure!(
                            binding["screen_id"] == geometry.screen_id
                                && binding["version"] == geometry.version
                                && binding["width"] == geometry.width
                                && binding["height"] == geometry.height
                                && geometry.width > 0
                                && geometry.height > 0,
                            "画面版本、屏幕或尺寸已失效，请读取新的媒体格式"
                        );
                        Shot {
                            owner: client.into(),
                            session: id.into(),
                            source: [geometry.width as usize, geometry.height as usize],
                            crop: [0, 0, geometry.width, geometry.height],
                            image: [geometry.width, geometry.height],
                            screen: screen_signature(s),
                            created: Instant::now(),
                        }
                    } else {
                        h.shots
                            .get(str_arg(args, "frame_id")?)
                            .context("截图引用已过期")?
                            .clone()
                    };
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
            if let Some(shot) = &shot
                && !matches!(action, "relative" | "scroll" | "touch")
            {
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
                "touch" => {
                    let phase = str_arg(args, "phase")?;
                    ensure!(
                        ["start", "move", "end", "cancel"].contains(&phase),
                        "触摸阶段无效"
                    );
                    let shot = shot.as_ref().unwrap();
                    let points = args["points"].as_array().context("缺少 points")?;
                    let points=points.iter().map(|point|{let (x,y)=shot.point(point["x"].as_f64().context("x 无效")?,point["y"].as_f64().context("y 无效")?)?;Ok(json!({"id":point["id"],"x":x,"y":y,"pressure":point["pressure"].as_f64().unwrap_or(0.5)}))}).collect::<Result<Vec<_>>>()?;
                    commands.push(json!({"op":"touch","phase":phase,"points":points}));
                }
                "relative" => {
                    let dx = args["dx"].as_i64().context("缺少 dx")?;
                    let dy = args["dy"].as_i64().context("缺少 dy")?;
                    ensure!(
                        dx.unsigned_abs() <= 32767 && dy.unsigned_abs() <= 32767,
                        "相对移动超出范围"
                    );
                    commands.push(json!({"op":"relative","dx":dx,"dy":dy}));
                }
                "button_down" | "button_up" => {
                    ensure!([1, 2, 16, 32, 64].contains(&button), "鼠标按钮无效");
                    commands.push(json!({"op":action,"button":button}));
                }
                "text" => commands.push(json!({"op":"text","text":str_arg(args,"text")?})),
                "release" => commands.push(json!({"op":"release"})),
                _ => bail!("未知键鼠操作"),
            }
            for mut command in commands {
                if let Some(shot) = &shot {
                    command["geometry"] = json!({"screen_id":shot.screen["id"],"version":shot.screen["version"],"width":shot.source[0],"height":shot.source[1]});
                }
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
        "uu_virtual_screen_create" | "uu_virtual_screen_remove" => {
            let mut command = args.clone();
            command["op"] = json!(if name.ends_with("create") {
                "virtual_display_create"
            } else {
                "virtual_display_remove"
            });
            start_job(hub, client, id, "display", move |h, c, s, j| {
                let _operation = Operation::acquire(&h, &s, "display")?;
                let cursor = h.lock().unwrap().ready(&c, &s)?.events.seq;
                let submitted = submit(&h, &c, &s, command)?;
                let request = wait_event(&h, &c, &s, cursor, Duration::from_secs(5), |v| {
                    v["kind"] == "virtual_display"
                        && v["data"]["phase"] == "submitted"
                        && v["data"]["client_request"] == submitted["request_id"]
                })?;
                job_update(
                    &h,
                    &j,
                    json!({"stage":"awaiting_screen_list","request":request["data"]}),
                );
                let result = wait_event(&h, &c, &s, cursor, Duration::from_secs(30), |v| {
                    v["kind"] == "virtual_display"
                        && v["data"]["request_id"] == request["data"]["request_id"]
                        && matches!(
                            v["data"]["phase"].as_str(),
                            Some("ready" | "failed" | "timeout")
                        )
                })?;
                ensure!(
                    result["data"]["phase"] == "ready",
                    "虚拟屏操作未完成：{}",
                    result["data"]
                );
                Ok(result["data"].clone())
            })
        }
        "uu_display_configure" => {
            let a = args.clone();
            start_job(hub, client, id, "display", move |h, c, s, j| {
                let _operation = Operation::acquire(&h, &s, "display")?;
                let cursor = h.lock().unwrap().ready(&c, &s)?.events.seq;
                if let Some(local) = a.get("local_decoder") {
                    submit(
                        &h,
                        &c,
                        &s,
                        json!({"op":"decoder","preference":local["preference"]}),
                    )?;
                }
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
                let mut remote_response = Value::Null;
                if [
                    "width",
                    "height",
                    "fps",
                    "quality",
                    "screen_id",
                    "bitrate",
                    "codec",
                    "true_color",
                    "dpi",
                    "hdr",
                ]
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
                    job_update(
                        &h,
                        &j,
                        json!({"stage":"remote_response","requested":a,"response":result["data"]}),
                    );
                    // A full CaptureSetting response may report a failure for
                    // one field after other fields already changed. Observe the
                    // requested effects and retain all remote diagnostics.
                    remote_response = result["data"].clone();
                    if let (Some(w), Some(height)) = (a["width"].as_i64(), a["height"].as_i64()) {
                        let start = Instant::now();
                        loop {
                            check_cancel(&h, &j)?;
                            let guard = h.lock().unwrap();
                            let session = guard.ready(&c, &s)?;
                            let sig = screen_signature(session);
                            let actual = session.frame.as_ref().is_some_and(|f| {
                                f.width as i64 == w
                                    && f.height as i64 == height
                                    && f.geometry == session.backend.media.geometry()
                            });
                            drop(guard);
                            if sig["remote"]["width"] == w
                                && sig["remote"]["height"] == height
                                && actual
                            {
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
                let started = Instant::now();
                loop {
                    check_cancel(&h, &j)?;
                    let guard = h.lock().unwrap();
                    let session = guard.ready(&c, &s)?;
                    let decoded = session.backend.media.decoder.lock().unwrap().clone();
                    let confirmed = session
                        .snapshot
                        .get("remote_capture")
                        .cloned()
                        .unwrap_or(Value::Null);
                    let screen = screen_signature(session);
                    let complete = a
                        .get("local_decoder")
                        .is_none_or(|local| decoded["preference"] == local["preference"])
                        && ["codec", "true_color", "hdr"].iter().all(|key| {
                            a.get(key)
                                .is_none_or(|requested| decoded[*key] == *requested)
                        })
                        && a.get("screen_id")
                            .is_none_or(|requested| screen["id"] == *requested)
                        && a.get("dpi")
                            .is_none_or(|requested| screen["remote"]["dpi"] == *requested)
                        && a.get("fps").is_none_or(|requested| {
                            requested.as_i64().is_some_and(|fps| {
                                confirmed["fps_enum"] == crate::core::wire::fps_enum(fps as i32)
                            })
                        })
                        && a.get("quality")
                            .is_none_or(|requested| confirmed["quality"] == *requested)
                        && a.get("bitrate")
                            .is_none_or(|requested| confirmed["max_bitrate"] == *requested);
                    drop(guard);
                    if complete {
                        break;
                    }
                    job_update(
                        &h,
                        &j,
                        json!({"stage":"awaiting_effective_settings","requested":a,"response":remote_response,"confirmed":confirmed,"screens":screen,"actual":decoded}),
                    );
                    ensure!(
                        started.elapsed() < Duration::from_secs(20),
                        "远端已接收设置，但未观测到所有请求参数生效；请检查任务中的 confirmed 和 actual"
                    );
                    std::thread::sleep(Duration::from_millis(50));
                }
                let h = h.lock().unwrap();
                let s = h.ready(&c, &s)?;
                Ok(
                    json!({"requested":a,"response":remote_response,"confirmed":s.snapshot.get("remote_capture"),"actual":s.backend.media.decoder.lock().unwrap().clone(),"screens":s.snapshot.get("screens"),"super_screen":s.snapshot.get("super_screen"),"completion_evidence":"observed_requested_effects"}),
                )
            })
        }
        "uu_clipboard_get" | "uu_clipboard_set" => {
            let _operation = Operation::acquire(hub, id, "clipboard")?;
            let _lease = ResourceGuard::acquire(hub, client, id, "operation")?;
            let cursor = hub.lock().unwrap().ready(client, id)?.events.seq;
            let get = name == "uu_clipboard_get";
            let scope = args["scope"].as_str().unwrap_or("remote");
            let format = args["format"].as_str().unwrap_or("text");
            ensure!(
                ["local", "remote"].contains(&scope),
                "scope 必须为 local 或 remote"
            );
            ensure!(
                ["text", "image", "files"].contains(&format),
                "此剪贴板格式尚未实现：{format}"
            );
            if format == "files" && scope == "remote" {
                let mut command = if get {
                    let destination = str_arg(args, "destination")?;
                    ensure!(
                        std::path::Path::new(destination).is_absolute()
                            && std::path::Path::new(destination).is_dir(),
                        "destination 须为现有本地接收目录"
                    );
                    json!({"op":"clipboard_get","format":"files","destination":destination})
                } else {
                    let paths = args["paths"].as_array().context("缺少 paths")?.clone();
                    ensure!(
                        !paths.is_empty()
                            && paths.len() <= 1000
                            && paths.iter().all(|p| p
                                .as_str()
                                .is_some_and(|p| std::path::Path::new(p).is_absolute())),
                        "paths 须为 1–1000 个本地绝对路径"
                    );
                    json!({"op":"clipboard_files","paths":paths})
                };
                if !get && let Some(drop) = args.get("drop") {
                    let mut h = hub.lock().unwrap();
                    let session = h.ready(client, id)?;
                    ensure!(
                        session
                            .input_owner
                            .as_deref()
                            .is_none_or(|owner| owner == client)
                            || session.input_until <= Instant::now(),
                        "键鼠当前由 GUI 或其他客户端使用"
                    );
                    let shot = h
                        .shots
                        .get(str_arg(args, "frame_id")?)
                        .context("拖放需要有效截图引用")?;
                    ensure!(
                        shot.owner == client
                            && shot.session == id
                            && shot.created.elapsed() < Duration::from_secs(120)
                            && shot.screen == screen_signature(session),
                        "拖放截图已失效"
                    );
                    let (x, y) = shot.point(
                        drop["x"].as_f64().context("drop.x 无效")?,
                        drop["y"].as_f64().context("drop.y 无效")?,
                    )?;
                    let geometry = session.backend.media.geometry();
                    let session = h.sessions.get_mut(id).unwrap();
                    if session
                        .input_owner
                        .as_deref()
                        .is_some_and(|owner| owner != client)
                    {
                        session.send(json!({"op":"release"}))?;
                    }
                    session.input_owner = Some(client.into());
                    session.input_until = Instant::now() + Duration::from_secs(3);
                    command["geometry"] = json!(geometry);
                    command["drop"] = json!({"x":x,"y":y,"screen_id":geometry.screen_id});
                }
                return start_job(hub, client, id, "transfer", move |h, c, s, j| {
                    files::clipboard_transfer(&h, &c, &s, &j, command)
                });
            }
            let mut command = json!({"op":if get{"clipboard_get"}else if scope=="local"{"clipboard_local"}else if format=="image"{"clipboard_image"}else{"clipboard"},"scope":scope,"format":format});
            if !get {
                if format == "files" {
                    command["paths"] = args["paths"].clone();
                } else if format == "text" {
                    command["text"] = json!(args["text"].as_str().context("缺少 text")?);
                } else {
                    command["data_base64"] = json!(
                        args["data_base64"]
                            .as_str()
                            .context("缺少 PNG data_base64")?
                    );
                }
            }
            let submitted = submit(hub, client, id, command)?;
            if scope == "local" {
                let event = wait_event(hub, client, id, cursor, Duration::from_secs(5), |v| {
                    v["kind"] == "local_clipboard_result"
                        && v["data"]["client_request"] == submitted["request_id"]
                })?;
                ensure!(
                    event["data"]["error"].is_null(),
                    "{}",
                    event["data"]["error"]
                );
                return Ok(event["data"]["value"].clone());
            }
            let image_request = if !get && format == "image" {
                Some(
                    wait_event(hub, client, id, cursor, Duration::from_secs(3), |v| {
                        v["kind"] == "clipboard_image_submitted"
                            && v["data"]["client_request"] == submitted["request_id"]
                    })?["data"]["request_id"]
                        .clone(),
                )
            } else {
                None
            };
            let event = wait_event(hub, client, id, cursor, Duration::from_secs(10), |v| {
                v["kind"] == "clipboard_error"
                    || (v["kind"]
                        == if get {
                            "clipboard_value"
                        } else if format == "image" {
                            "clipboard_image_offered"
                        } else {
                            "clipboard_sent"
                        }
                        && (!get || v["data"]["format"] == format)
                        && image_request
                            .as_ref()
                            .is_none_or(|id| v["data"]["request_id"] == *id))
            })?;
            ensure!(
                event["kind"] != "clipboard_error",
                "{}",
                event["data"]["message"]
            );
            if !get {
                ensure!(event["data"]["error"] == 1, "远端未确认剪贴板写入");
            }
            Ok(if get {
                event["data"].clone()
            } else if format == "image" {
                json!({"source":"remote","format":"image","state":"offered","remote_accepted_format":true,"content_transfer":"on_remote_request","completed":false})
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

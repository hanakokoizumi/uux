use super::{
    cloud::{Cloud, Target},
    session::{Output, Session},
};
use crate::backend::{Device, Event};
use anyhow::{Context, Result, ensure};
use base64::Engine;
use serde_json::Value;
use std::time::Duration;
use tokio::{sync::mpsc, task::JoinHandle};
use tokio_util::sync::CancellationToken;

fn state(cloud: &Cloud, output: &Output) {
    let (logged_in, nickname) = cloud.public_state();
    output.event(Event::State {
        logged_in,
        nickname,
    });
}
fn error(output: &Output, scope: &str, result: Result<()>, login_id: Option<u64>) {
    if let Err(e) = result {
        output.event(Event::Error {
            scope: scope.into(),
            message: format!("{e}"),
            generation: None,
            login_id,
        });
    }
}
async fn devices(cloud: &Cloud, output: &Output) -> Result<()> {
    let raw = cloud.devices().await?;
    let mut devices = vec![];
    for (group, shared) in [
        ("my_binded_devices", false),
        ("others_shared_devices", true),
    ] {
        for item in raw[group].as_array().context("设备分组格式无效")? {
            let d = item.get("device").unwrap_or(item);
            let Some(id) = d["device_id"].as_str() else {
                continue;
            };
            if id == cloud.device_id() || devices.iter().any(|d: &Device| d.id == id) {
                continue;
            }
            devices.push(Device {
                id: id.into(),
                name: d["alias"].as_str().unwrap_or("未命名设备").into(),
                online: d["status"] == "CONNECTED" && d["controllable"] == true,
                shared,
                version: d["version_name"].as_str().unwrap_or("").into(),
                wallpaper: d["wallpaper_url"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned),
            });
        }
    }
    devices.sort_by_key(|d| (!d.online, d.name.clone()));
    output.event(Event::Devices { devices });
    Ok(())
}
async fn login(cloud: Cloud, output: Output, id: u64) -> Result<()> {
    let qr = cloud.qr().await?;
    let url = qr["qrcode_jump_url"].as_str().context("缺少二维码地址")?;
    let token = qr["token"].as_str().context("缺少二维码凭据")?;
    let bitmap = qrcode::QrCode::new(url)?
        .render::<image::Luma<u8>>()
        .min_dimensions(280, 280)
        .build();
    let mut png = std::io::Cursor::new(vec![]);
    image::DynamicImage::ImageLuma8(bitmap).write_to(&mut png, image::ImageFormat::Png)?;
    output.event(Event::Qr {
        login_id: id,
        png: base64::engine::general_purpose::STANDARD.encode(png.into_inner()),
    });
    let mut status = 1;
    loop {
        status = cloud.poll_qr(url, token, status).await?;
        ensure!((1..=4).contains(&status), "扫码状态无效");
        output.event(Event::LoginStatus {
            login_id: id,
            status: status as u32,
        });
        ensure!(status != 3, "手机端已取消登录");
        if status == 4 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    cloud.login(url, token).await?;
    state(&cloud, &output);
    output.event(Event::LoginComplete { login_id: id });
    devices(&cloud, &output).await
}
struct Active {
    generation: u64,
    cancel: CancellationToken,
    commands: mpsc::Sender<Value>,
    task: JoinHandle<()>,
}
async fn stop(active: &mut Option<Active>) {
    if let Some(a) = active.take() {
        a.cancel.cancel();
        let _ = a.task.await;
    }
}
pub async fn run(cloud: Cloud, output: Output, mut commands: mpsc::Receiver<Value>) {
    let mut cloud = cloud;
    output.event(Event::Hello { protocol: 1 });
    state(&cloud, &output);
    let mut active: Option<Active> = None;
    let mut login_task: Option<JoinHandle<()>> = None;
    let mut refresh: Option<JoinHandle<()>> = None;
    let mut preview: Option<JoinHandle<()>> = None;
    let mut device_action: Option<JoinHandle<()>> = None;
    while let Some(command) = commands.recv().await {
        match command["op"].as_str().unwrap_or("") {
            "device_action" => {
                if device_action
                    .as_ref()
                    .is_some_and(|task| !task.is_finished())
                {
                    output.native("device_action_result",serde_json::json!({"request_id":command["request_id"],"ok":false,"message":"设备操作正在处理中"}));
                    continue;
                }
                let cloud = cloud.clone();
                let output = output.clone();
                device_action = Some(tokio::spawn(async move {
                    let result = super::device_actions::perform(
                        &cloud,
                        command["device_id"].as_str().unwrap_or(""),
                        command["action"].as_str().unwrap_or(""),
                        command["name"].as_str().unwrap_or(""),
                    )
                    .await;
                    output.native(
                        "device_action_result",
                        serde_json::json!({"request_id":command["request_id"],"ok":result.is_ok(),
                        "message":result.err().map(|e| e.to_string())}),
                    );
                }));
            }
            "proxy_apply" | "proxy_test" => {
                let config = serde_json::from_value::<super::cloud::ProxySettings>(
                    command["settings"].clone(),
                );
                match config.ok().and_then(|s| cloud.with_proxy(&s).ok()) {
                    Some(candidate) if command["op"] == "proxy_apply" => {
                        cloud = candidate;
                        output.native(
                            "network_result",
                            serde_json::json!({"message":"代理设置已应用","ok":true}),
                        );
                    }
                    Some(candidate) => {
                        let output = output.clone();
                        tokio::spawn(async move {
                            let result = candidate.test_network().await;
                            output.native("network_result",serde_json::json!({"ok":result.is_ok(),
                                "message":result.err().map_or("连接测试成功".into(),|e|e.to_string())}));
                        });
                    }
                    None => output.native(
                        "network_result",
                        serde_json::json!({"ok":false,"message":"代理配置无效，请检查地址及端口"}),
                    ),
                }
            }
            "quit" => break,
            "status" => state(&cloud, &output),
            "preview" => {
                if let Some(task) = preview.take() {
                    task.abort();
                }
                let cloud = cloud.clone();
                let output = output.clone();
                let url = command["url"].as_str().unwrap_or("").to_owned();
                let device_id = command["device_id"].as_str().unwrap_or("").to_owned();
                preview = Some(tokio::spawn(async move {
                    if let Ok(image) = cloud.preview(&url).await {
                        output.event(Event::Preview { device_id, image });
                    }
                }));
            }
            "devices" => {
                if let Some(task) = refresh.take() {
                    task.abort();
                }
                let cloud = cloud.clone();
                let output = output.clone();
                refresh = Some(tokio::spawn(async move {
                    error(&output, "devices", devices(&cloud, &output).await, None);
                }));
            }
            "login" => {
                stop(&mut active).await;
                if let Some(task) = refresh.take() {
                    task.abort();
                }
                if let Some(task) = login_task.take() {
                    task.abort();
                }
                let id = command["login_id"].as_u64().unwrap_or(1);
                let cloud = cloud.clone();
                let output = output.clone();
                login_task = Some(tokio::spawn(async move {
                    let result = tokio::time::timeout(
                        Duration::from_secs(300),
                        login(cloud, output.clone(), id),
                    )
                    .await;
                    error(
                        &output,
                        "login",
                        result.unwrap_or_else(|_| Err(anyhow::anyhow!("二维码已过期"))),
                        Some(id),
                    );
                }));
            }
            "cancel_login" => {
                if let Some(task) = login_task.take() {
                    task.abort();
                }
            }
            "logout" => {
                if let Some(task) = device_action.take() {
                    let _ = task.await;
                }
                stop(&mut active).await;
                if let Some(task) = refresh.take() {
                    task.abort();
                }
                if let Some(task) = login_task.take() {
                    task.abort();
                }
                error(&output, "logout", cloud.logout(), None);
                state(&cloud, &output);
            }
            "connect" => {
                stop(&mut active).await;
                if let Some(task) = login_task.take() {
                    task.abort();
                }
                let generation = command["generation"].as_u64().unwrap_or(1);
                let target = if let Some(id) = command["assist_id"].as_str() {
                    Target::Assistance {
                        id: id.into(),
                        code: command["assist_code"].as_str().unwrap_or("").into(),
                    }
                } else {
                    Target::Device(command["device_id"].as_str().unwrap_or("").to_owned())
                };
                let cancel = CancellationToken::new();
                let session = Session::new(generation, output.clone(), cancel.clone());
                let (tx, rx) = mpsc::channel(512);
                let cloud = cloud.clone();
                let task =
                    tokio::spawn(session.run(cloud, target, command["settings"].clone(), rx));
                active = Some(Active {
                    generation,
                    cancel,
                    commands: tx,
                    task,
                });
            }
            "disconnect" => stop(&mut active).await,
            "input" => {
                if let Some(a) = &active
                    && command["generation"].as_u64() == Some(a.generation)
                    && !a.task.is_finished()
                    && a.commands.try_send(command["command"].clone()).is_err()
                {
                    a.cancel.cancel();
                    error(
                        &output,
                        "input",
                        Err(anyhow::anyhow!("输入队列已停止，会话正在退出")),
                        None,
                    );
                }
            }
            _ => {}
        }
    }
    if let Some(task) = login_task {
        task.abort();
    }
    if let Some(task) = refresh {
        task.abort();
    }
    if let Some(task) = preview {
        task.abort();
    }
    if let Some(task) = device_action {
        let _ = task.await;
    }
    stop(&mut active).await;
}

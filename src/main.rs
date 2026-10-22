mod app;
mod backend;
mod brand;
mod capabilities;
mod capture_state;
mod core;
mod diagnostic_io;
mod input;
mod instance;
mod ipc;
#[cfg(test)]
mod live_test;
mod mcp;
mod mcp_api;
mod media_bus;
mod perf;
mod platform;
mod preferences_store;
mod service;
mod stream_decode;
mod streams;

use std::{path::PathBuf, time::Duration};

#[derive(Default)]
pub struct Options {
    mcp: bool,
    service: bool,
    probe: bool,
    diagnostic_stdin: bool,
    state_dir: Option<PathBuf>,
    connect: Option<String>,
    login: bool,
    files: bool,
    view: Option<String>,
    screenshot: Option<PathBuf>,
    window_test: Option<PathBuf>,
    quit_after: Option<Duration>,
}

fn options() -> Result<Options, String> {
    let mut result = Options::default();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--mcp" {
            result.mcp = true;
            continue;
        }
        if arg == "--service" {
            result.service = true;
            continue;
        }
        if arg == "--probe" {
            result.probe = true;
            continue;
        }
        if arg == "--diagnostic-stdin" {
            result.diagnostic_stdin = true;
            continue;
        }
        if arg == "--login" {
            result.login = true;
            continue;
        }
        if arg == "--files" {
            result.files = true;
            continue;
        }
        if arg == "--help" || arg == "-h" {
            println!(
                "UUX — Linux 原生远程主控\n\
                --mcp               MCP stdio 服务，与 GUI 共用原生连接\n\
                --probe             无窗口诊断，标准输入接收 JSON 命令\n\
                --diagnostic-stdin  GUI 诊断：输入 input/quit 命令，输出事件及周期性远端帧截图\n\
                --state-dir PATH    独立登录状态目录\n\
                --connect ID        启动后连接指定设备\n\
                --login             启动后显示登录二维码\n\
                --files             连接成功后打开文件传输窗口\n\
                --view PAGE         初始页面：all/device/assistance/favorites/settings/security/keyboard/network/picture/mcp；界面检查：favorite-add/rename/ports\n\
                --screenshot PATH   保存本应用窗口截图（诊断）\n\
                --window-test PATH  验证独立窗口的会话生命周期，保存 JSON 报告（需 --connect）\n\
                --quit-after N      N 秒后正常退出（诊断）"
            );
            std::process::exit(0);
        }
        let value = args.next().ok_or_else(|| format!("{arg} 缺少参数"))?;
        match arg.as_str() {
            "--state-dir" => result.state_dir = Some(PathBuf::from(value)),
            "--connect" => result.connect = Some(value),
            "--screenshot" => result.screenshot = Some(PathBuf::from(value)),
            "--window-test" => {
                result.window_test = Some(PathBuf::from(value));
                result.files = true;
            }
            "--view" => {
                if ![
                    "all",
                    "assistance",
                    "device",
                    "favorites",
                    "favorite-add",
                    "rename",
                    "ports",
                    "settings",
                    "security",
                    "network",
                    "keyboard",
                    "picture",
                    "mcp",
                ]
                .contains(&value.as_str())
                {
                    return Err("未知页面".into());
                }
                result.view = Some(value);
            }
            "--quit-after" => {
                let seconds: f64 = value.parse().map_err(|_| "退出时间必须是正数")?;
                if !seconds.is_finite() || seconds <= 0.0 || seconds > 86400.0 {
                    return Err("退出时间必须在 0–86400 秒之间".into());
                }
                result.quit_after = Some(Duration::from_secs_f64(seconds));
            }
            _ => return Err(format!("未知参数：{arg}")),
        }
    }
    for path in [&mut result.state_dir].into_iter().flatten() {
        if path.is_relative() {
            *path = std::env::current_dir()
                .map_err(|_| "无法读取当前目录")?
                .join(&*path);
        }
    }
    if result.window_test.is_some() {
        if result.connect.is_none() {
            return Err("窗口测试需要 --connect 设备ID".into());
        }
        if result.quit_after.is_none() {
            result.quit_after = Some(Duration::from_secs(60));
        }
    }
    Ok(result)
}

fn main() -> eframe::Result {
    let opts = options().unwrap_or_else(|error| {
        eprintln!("{error}");
        std::process::exit(2);
    });
    perf::init_transport_diagnostics();
    if opts.service || opts.mcp {
        let result = if opts.service {
            service::run(opts.state_dir.as_deref())
        } else {
            mcp::run(opts.state_dir.as_deref())
        };
        if let Err(e) = result {
            eprintln!("{e:#}");
            std::process::exit(1);
        }
        return Ok(());
    }
    if opts.probe {
        probe(opts);
        return Ok(());
    }
    let instance = instance::Instance::acquire(opts.state_dir.as_deref()).unwrap_or_else(|e| {
        eprintln!("{e:#}");
        std::process::exit(1);
    });
    let Some(instance) = instance else {
        return Ok(());
    };
    // Preserve the existing preference while updating the executable and name.
    if platform::autostart_enabled()
        && let Err(error) = platform::set_autostart(true)
    {
        eprintln!("更新 UUX 自启动入口失败：{error:#}");
    }
    let native = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([920.0, 680.0])
            .with_min_inner_size([860.0, 600.0])
            .with_decorations(false)
            .with_icon(brand::icon())
            .with_active(opts.screenshot.is_none() || opts.diagnostic_stdin)
            .with_app_id("uux"),
        renderer: eframe::Renderer::Glow,
        // Forward physical shortcuts to the host. Text clipboard synchronization
        // is handled by the native clipboard worker.
        clipboard_shortcuts: false,
        ..Default::default()
    };
    eframe::run_native(
        "UUX",
        native,
        Box::new(move |cc| {
            instance.activate_with(cc.egui_ctx.clone());
            Ok(Box::new(app::Desktop::new(cc, opts)))
        }),
    )
}

fn probe(opts: Options) {
    use serde_json::json;
    use std::{thread, time::Instant};
    let mut backend =
        backend::Backend::start(opts.state_dir.as_deref(), eframe::egui::Context::default())
            .unwrap();
    if opts.login {
        backend.send(json!({"op":"login","login_id":1})).unwrap();
    } else if let Some(id) = opts.connect {
        backend
            .send(json!({"op":"connect","device_id":id,"generation":1}))
            .unwrap();
    } else {
        backend.send(json!({"op":"devices"})).unwrap();
    }
    let mut diagnostics = diagnostic_io::DiagnosticIo::start(eframe::egui::Context::default());
    let start = Instant::now();
    'probe: loop {
        for event in backend.events.try_iter() {
            println!("{}", serde_json::to_string(&event).unwrap());
        }
        let commands: Vec<_> = diagnostics.commands.try_iter().collect();
        for command in commands {
            if command["op"] == "quit" {
                break 'probe;
            }
            diagnostics.observe_command(&command);
            if let Err(error) = backend.send(command) {
                eprintln!("{error}");
                break 'probe;
            }
        }
        let frame = backend.latest.lock().unwrap().take();
        diagnostics.observe_frame(frame.as_ref());
        if let Some(frame) = frame {
            diagnostics.snapshot(&frame, opts.screenshot.as_ref());
        }
        if start.elapsed() > opts.quit_after.unwrap_or(Duration::from_secs(20)) {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    backend.shutdown();
    for event in backend.events.try_iter() {
        println!("{}", serde_json::to_string(&event).unwrap());
    }
}

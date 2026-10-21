use anyhow::{Context, Result, ensure};
use std::path::PathBuf;

fn autostart_path() -> Result<PathBuf> {
    Ok(directories::BaseDirs::new()
        .context("无法读取配置目录")?
        .config_dir()
        .join("autostart/uux.desktop"))
}
pub fn autostart_enabled() -> bool {
    autostart_path()
        .is_ok_and(|p| p.exists() || p.with_file_name("uu-linux-desktop.desktop").exists())
}
pub fn set_autostart(enabled: bool) -> Result<()> {
    let path = autostart_path()?;
    let legacy = path.with_file_name("uu-linux-desktop.desktop");
    if !enabled {
        for entry in [&path, &legacy] {
            if entry.exists() {
                std::fs::remove_file(entry)?;
            }
        }
        return Ok(());
    }
    let exe = std::env::current_exe()?.canonicalize()?;
    let exe = exe.to_str().context("可执行文件路径不是 UTF-8")?;
    ensure!(!exe.contains(['\n', '\r']), "可执行文件路径无效");
    let escaped = exe
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('`', "\\`")
        .replace('$', "\\$")
        .replace('%', "%%");
    std::fs::create_dir_all(path.parent().unwrap())?;
    std::fs::write(
        &path,
        format!(
            "[Desktop Entry]\nType=Application\nName=UUX\nComment=Linux 原生远程主控\nExec=\"{escaped}\"\nTerminal=false\nX-GNOME-Autostart-enabled=true\n"
        ),
    )?;
    if legacy.exists() {
        std::fs::remove_file(legacy)?;
    }
    Ok(())
}

/// Login1 owns the suspend inhibitor through the file descriptor lifetime.
/// A separate screensaver cookie covers idle blanking on non-systemd desktops.
pub struct Inhibitor {
    _sleep: Option<zbus::zvariant::OwnedFd>,
    saver: Option<(zbus::Connection, u32)>,
}
impl Inhibitor {
    pub async fn acquire() -> Result<Self> {
        let sleep = async {
            let bus = zbus::Connection::system().await?;
            let proxy = zbus::Proxy::new(
                &bus,
                "org.freedesktop.login1",
                "/org/freedesktop/login1",
                "org.freedesktop.login1.Manager",
            )
            .await?;
            proxy
                .call::<_, _, zbus::zvariant::OwnedFd>(
                    "Inhibit",
                    &("sleep:idle", "UUX", "远程控制进行中", "block"),
                )
                .await
        };
        let saver = async {
            let bus = zbus::Connection::session().await?;
            let proxy = zbus::Proxy::new(
                &bus,
                "org.freedesktop.ScreenSaver",
                "/org/freedesktop/ScreenSaver",
                "org.freedesktop.ScreenSaver",
            )
            .await?;
            let cookie: u32 = proxy.call("Inhibit", &("UUX", "远程控制进行中")).await?;
            Ok::<_, zbus::Error>((bus, cookie))
        };
        let (sleep, saver) = tokio::join!(
            tokio::time::timeout(std::time::Duration::from_secs(3), sleep),
            tokio::time::timeout(std::time::Duration::from_secs(3), saver)
        );
        let result = Self {
            _sleep: sleep.ok().and_then(Result::ok),
            saver: saver.ok().and_then(Result::ok),
        };
        ensure!(
            result._sleep.is_some() || result.saver.is_some(),
            "桌面未提供可用的休眠抑制服务"
        );
        Ok(result)
    }
    pub async fn release(mut self) {
        if let Some((bus, cookie)) = self.saver.take()
            && let Ok(proxy) = zbus::Proxy::new(
                &bus,
                "org.freedesktop.ScreenSaver",
                "/org/freedesktop/ScreenSaver",
                "org.freedesktop.ScreenSaver",
            )
            .await
        {
            let _ = tokio::time::timeout(
                std::time::Duration::from_secs(1),
                proxy.call::<_, _, ()>("UnInhibit", &(cookie,)),
            )
            .await;
        }
    }
}

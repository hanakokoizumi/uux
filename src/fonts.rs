use eframe::egui;
use std::{path::PathBuf, process::Command};

fn output(program: &str, args: &[&str]) -> Option<String> {
    let result = Command::new(program).args(args).output().ok()?;
    result
        .status
        .success()
        .then(|| String::from_utf8_lossy(&result.stdout).trim().to_owned())
}

fn setting(path: PathBuf, section: &str, key: &str) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut active = false;
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            active = line == section;
        }
        if active
            && let Some((name, value)) = line.split_once('=')
            && name == key
            && !value.is_empty()
        {
            return Some(value.to_owned());
        }
    }
    None
}

fn pango_family(value: &str) -> String {
    let value = value.trim().trim_matches('\'');
    let family = value
        .rsplit_once(' ')
        .filter(|(_, size)| size.parse::<f32>().is_ok())
        .map_or(value, |(family, _)| family);
    family.to_owned()
}

/// Prefer the active desktop's UI font, then the standard fontconfig alias.
/// Font selection never assumes KDE, a particular distribution, or a CJK family.
fn system_family() -> String {
    let desktop = std::env::var("XDG_CURRENT_DESKTOP")
        .unwrap_or_default()
        .to_lowercase();
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| directories::BaseDirs::new().map(|d| d.config_dir().to_owned()));
    if desktop.split(':').any(|d| d == "kde")
        && let Some(config) = &config
        && let Some(font) = setting(config.join("kdeglobals"), "[General]", "font")
    {
        return font.split(',').next().unwrap_or("sans-serif").to_owned();
    }
    if desktop.contains("xfce")
        && let Some(font) = output("xfconf-query", &["-c", "xsettings", "-p", "/Gtk/FontName"])
    {
        return pango_family(&font);
    }
    let schema = if desktop.contains("cinnamon") {
        Some("org.cinnamon.desktop.interface")
    } else if desktop.contains("mate") {
        Some("org.mate.interface")
    } else if desktop.contains("gnome") || desktop.contains("unity") || desktop.contains("budgie") {
        Some("org.gnome.desktop.interface")
    } else {
        None
    };
    if let Some(schema) = schema
        && let Some(font) = output("gsettings", &["get", schema, "font-name"])
    {
        return pango_family(&font);
    }
    // Portals also cover compositors without a conventional desktop environment.
    if let Some(value) = output(
        "gdbus",
        &[
            "call",
            "--session",
            "--timeout",
            "1",
            "--dest",
            "org.freedesktop.portal.Desktop",
            "--object-path",
            "/org/freedesktop/portal/desktop",
            "--method",
            "org.freedesktop.portal.Settings.Read",
            "org.gnome.desktop.interface",
            "font-name",
        ],
    ) && let Some(font) = value.split('\'').nth(1)
    {
        return pango_family(font);
    }
    if let Some(config) = config {
        for path in ["gtk-4.0/settings.ini", "gtk-3.0/settings.ini"] {
            if let Some(font) = setting(config.join(path), "[Settings]", "gtk-font-name") {
                return pango_family(&font);
            }
        }
    }
    "sans-serif".into()
}

fn font(pattern: &str) -> Option<egui::FontData> {
    let resolved = output("fc-match", &["-f", "%{file}\n%{index}\n", pattern])?;
    let mut lines = resolved.lines();
    let mut data = egui::FontData::from_owned(std::fs::read(lines.next()?).ok()?);
    data.index = lines.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    Some(data)
}

pub fn setup(ctx: &egui::Context) {
    let family = system_family();
    let mut fonts = egui::FontDefinitions::default();
    for (name, pattern) in [
        ("system-ui", family.clone()),
        ("system-ui-cjk", "sans-serif:lang=zh".into()),
    ] {
        if let Some(data) = font(&pattern) {
            fonts.font_data.insert(name.into(), data.into());
        }
    }
    let mut primary: Vec<String> = ["system-ui", "system-ui-cjk"]
        .into_iter()
        .filter(|name| fonts.font_data.contains_key(*name))
        .map(str::to_owned)
        .collect();
    primary.extend(
        fonts
            .families
            .get(&egui::FontFamily::Proportional)
            .cloned()
            .unwrap_or_default(),
    );
    fonts
        .families
        .insert(egui::FontFamily::Proportional, primary.clone());
    // Terminal cells keep a monospace Latin face with the system's CJK fallback.
    fonts
        .families
        .entry(egui::FontFamily::Monospace)
        .or_default()
        .extend(primary.iter().take(2).cloned());
    if let Some(data) = font(&format!("{family}:weight=bold")) {
        fonts.font_data.insert("system-ui-bold".into(), data.into());
        primary.insert(0, "system-ui-bold".into());
    }
    fonts
        .families
        .insert(egui::FontFamily::Name("system-ui-bold".into()), primary);
    ctx.set_fonts(fonts);
}

pub fn bold(size: f32) -> egui::FontId {
    egui::FontId::new(size, egui::FontFamily::Name("system-ui-bold".into()))
}

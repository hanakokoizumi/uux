//! Merge only changed preference keys so GUI and MCP edits do not overwrite each other.
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use std::{
    path::Path,
    time::{Duration, Instant},
};
pub fn read(path: &Path) -> Value {
    std::fs::read(path)
        .ok()
        .and_then(|v| serde_json::from_slice(&v).ok())
        .filter(Value::is_object)
        .unwrap_or(json!({}))
}
pub fn merge(path: &Path, previous: &Value, current: &Value) -> Result<Value> {
    let lock_path = path.with_extension("lock");
    let start = Instant::now();
    let _guard = loop {
        if let Some(lock) = crate::ipc::lock(&lock_path)? {
            break lock;
        }
        ensure!(
            start.elapsed() < Duration::from_secs(2),
            "配置正在被其他窗口修改"
        );
        std::thread::sleep(Duration::from_millis(5));
    };
    let mut saved = read(path);
    for (key, value) in current.as_object().into_iter().flatten() {
        if previous.get(key) != Some(value) {
            saved[key] = value.clone();
        }
    }
    crate::core::cloud::write_private(path, &saved)?;
    Ok(saved)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unrelated_gui_and_mcp_changes_are_preserved() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("desktop.json");
        let base = json!({"fps":60,"favorite_devices":[]});
        merge(&p, &json!({}), &base).unwrap();
        merge(
            &p,
            &base,
            &json!({"fps":60,"favorite_devices":[{"id":"123456789"}]}),
        )
        .unwrap();
        let latest = merge(&p, &base, &json!({"fps":30,"favorite_devices":[]})).unwrap();
        assert_eq!(latest["fps"], 30);
        assert_eq!(latest["favorite_devices"][0]["id"], "123456789");
    }
}

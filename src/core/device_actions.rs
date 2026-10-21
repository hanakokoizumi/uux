use super::cloud::Cloud;
use anyhow::{Result, ensure};
use reqwest::Method;
use serde_json::{Value, json};

fn valid_device_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Explicit endpoints recovered from GameViewer's authenticated device APIs.
pub fn unsigned_endpoint(method: &Method, path: &str) -> bool {
    if let Some(rest) = path.strip_prefix("/api/v1/device/share/") {
        return method == Method::POST && rest.strip_suffix("/unbind").is_some_and(valid_device_id);
    }
    path.strip_prefix("/api/v1/device/").is_some_and(|rest| {
        method == Method::PUT && valid_device_id(rest)
            || method == Method::POST
                && (rest.strip_suffix("/unbind").is_some_and(valid_device_id)
                    || rest.strip_suffix("/reboot").is_some_and(valid_device_id))
    })
}

fn request(
    id: &str,
    op: &str,
    name: &str,
    shared: bool,
) -> Result<(Method, String, Option<Value>)> {
    ensure!(valid_device_id(id), "设备 ID 无效");
    Ok(match op {
        "rename" => {
            let name = name.trim();
            ensure!(
                !name.is_empty()
                    && name.chars().count() <= 40
                    && !name.chars().any(char::is_control),
                "设备名称须为 1–40 个字符"
            );
            (
                Method::PUT,
                format!("/api/v1/device/{id}"),
                Some(json!({"alias":name})),
            )
        }
        "remove" => (
            Method::POST,
            format!(
                "/api/v1/device/{}{id}/unbind",
                if shared { "share/" } else { "" }
            ),
            None,
        ),
        "reboot" => (Method::POST, format!("/api/v1/device/{id}/reboot"), None),
        _ => anyhow::bail!("不支持的设备操作"),
    })
}

pub async fn perform(cloud: &Cloud, id: &str, op: &str, name: &str) -> Result<()> {
    ensure!(
        valid_device_id(id) && id != cloud.device_id(),
        "不能操作本机设备"
    );
    // Re-read the authenticated list before mutations: never trust a UI-supplied
    // ownership flag or stale presence when unbinding a device.
    let devices = cloud.devices().await?;
    let mut target = None;
    for (group, shared) in [
        ("my_binded_devices", false),
        ("others_shared_devices", true),
    ] {
        for item in devices[group].as_array().into_iter().flatten() {
            let device = item.get("device").unwrap_or(item);
            if device["device_id"] == id {
                target = Some((device, shared));
                break;
            }
        }
    }
    let (device, shared) =
        target.ok_or_else(|| anyhow::anyhow!("设备已不在当前账号列表中，请刷新"))?;
    if op == "remove" {
        ensure!(
            device["status"] != "CONNECTED",
            "在线设备不能删除，请先在该设备退出登录"
        );
    }
    if op == "reboot" {
        ensure!(device["status"] == "CONNECTED", "设备已离线");
    }
    let (method, path, body) = request(id, op, name, shared)?;
    cloud.request(method, &path, body, true).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn operations_use_verified_paths_and_reject_path_injection() {
        let (method, path, body) = request("abc_123", "rename", " 新名称 ", false).unwrap();
        assert_eq!(method, Method::PUT);
        assert_eq!(path, "/api/v1/device/abc_123");
        assert_eq!(body, Some(json!({"alias":"新名称"})));
        assert_eq!(
            request("abc", "remove", "", true).unwrap().1,
            "/api/v1/device/share/abc/unbind"
        );
        assert!(request("../other", "remove", "", false).is_err());
        assert!(request("abc", "rename", "", false).is_err());
        assert!(!unsigned_endpoint(&Method::PUT, "/api/v1/device/abc/other"));
        assert!(!unsigned_endpoint(
            &Method::POST,
            "/api/v1/device/share/../unbind"
        ));
        assert!(!unsigned_endpoint(&Method::GET, "/api/v1/device/abc"));
    }
}

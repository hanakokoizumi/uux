use rmcp::model::{Tool, ToolAnnotations};
use serde_json::{Value, json};
fn string() -> Value {
    json!({"type":"string"})
}
fn integer() -> Value {
    json!({"type":"integer"})
}
fn boolean() -> Value {
    json!({"type":"boolean"})
}
fn enumeration(values: &[&str]) -> Value {
    json!({"type":"string","enum":values})
}
fn fields(items: &[(&str, Value)]) -> Value {
    json!(
        items
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect::<serde_json::Map<_, _>>()
    )
}
fn add(
    tools: &mut Vec<Tool>,
    name: &str,
    description: &str,
    required: &[&str],
    props: Value,
    read_only: bool,
) {
    let schema = json!({"type":"object","properties":props,"required":required,"additionalProperties":false});
    let mut tool = Tool::new(
        name.to_owned(),
        description.to_owned(),
        schema.as_object().unwrap().clone(),
    );
    tool.annotations = Some(
        ToolAnnotations::default()
            .read_only(read_only)
            .destructive(!read_only),
    );
    tools.push(tool);
}
fn session(items: &[(&str, Value)]) -> Value {
    let mut v = fields(items);
    v["session_id"] = string();
    v
}
pub fn catalog() -> Vec<Tool> {
    let mut t = vec![];
    for (name, desc) in [
        ("uu_account_status", "查询原生 UU 账号登录状态，不返回凭据"),
        (
            "uu_login_start",
            "创建扫码登录请求；用 uu_login_status 取得二维码与结果",
        ),
        ("uu_login_status", "获取当前扫码二维码图像和确认状态"),
        ("uu_login_cancel", "取消当前扫码登录"),
        (
            "uu_devices_list",
            "刷新并列出账号设备、在线状态及连接用 device_id",
        ),
        (
            "uu_session_list",
            "列出共享核心中的连接和本客户端是否已加入",
        ),
    ] {
        add(
            &mut t,
            name,
            desc,
            &[],
            json!({}),
            !matches!(name, "uu_login_start" | "uu_login_cancel"),
        );
    }
    add(
        &mut t,
        "uu_session_open",
        "建立或复用原生 UU 会话。device_id 来自设备列表；协助使用 assist_id 和 assist_code。tools 模式无需等待画面。",
        &[],
        fields(&[
            ("device_id", string()),
            ("assist_id", string()),
            ("assist_code", string()),
            ("session_id", string()),
            ("mode", enumeration(&["tools", "desktop"])),
            ("super_screen", boolean()),
            ("wait_ms", integer()),
        ]),
        false,
    );
    for (name, desc) in [
        ("uu_session_status", "查询连接阶段、工具及画面就绪状态"),
        (
            "uu_session_close",
            "释放本客户端使用权；保留其他 GUI/MCP 使用方的连接",
        ),
        (
            "uu_screens_list",
            "查询远端显示器、分辨率、超级屏和采集参数",
        ),
    ] {
        add(
            &mut t,
            name,
            desc,
            &["session_id"],
            session(&[]),
            name != "uu_session_close",
        );
    }
    add(
        &mut t,
        "uu_screenshot",
        "返回远端 PNG 图像和 frame_id。键鼠坐标以本次返回图像像素为准，裁剪坐标以完整视频帧像素为准。",
        &["session_id"],
        session(&[
            ("max_width", integer()),
            ("wait_ms", integer()),
            (
                "crop",
                json!({"type":"object","properties":fields(&[("x",integer()),("y",integer()),("width",integer()),("height",integer())]),"required":["x","y","width","height"],"additionalProperties":false}),
            ),
        ]),
        true,
    );
    add(
        &mut t,
        "uu_desktop_action",
        "远端键鼠。位置操作须提供 frame_id,x,y；drag 另需 to_x,to_y。keys 接受 Ctrl/Alt/Enter/F1 等名称或 Windows VK。scroll 增量为 120 单位/格。",
        &["session_id", "action"],
        session(&[
            (
                "action",
                enumeration(&[
                    "move",
                    "click",
                    "double_click",
                    "right_click",
                    "drag",
                    "scroll",
                    "key",
                    "key_down",
                    "key_up",
                    "release",
                ]),
            ),
            ("frame_id", string()),
            ("x", json!({"type":"number"})),
            ("y", json!({"type":"number"})),
            ("to_x", json!({"type":"number"})),
            ("to_y", json!({"type":"number"})),
            ("button", integer()),
            ("dx", integer()),
            ("dy", integer()),
            (
                "keys",
                json!({"type":"array","items":{"oneOf":[{"type":"string"},{"type":"integer"}]}}),
            ),
        ]),
        false,
    );
    add(
        &mut t,
        "uu_type_text",
        "向远端当前输入焦点提交 Unicode 文字，最多 64 KiB",
        &["session_id", "text"],
        session(&[("text", string())]),
        false,
    );
    add(
        &mut t,
        "uu_display_configure",
        "修改显示器、超级屏、分辨率、帧率或画质，返回异步 task_id；用任务结果确认生效。",
        &["session_id"],
        session(&[
            ("screen_id", integer()),
            ("super_screen", boolean()),
            ("width", integer()),
            ("height", integer()),
            ("fps", json!({"type":"integer","enum":[30,60,90,144]})),
            ("quality", json!({"type":"integer","minimum":1,"maximum":6})),
            ("bitrate", integer()),
        ]),
        false,
    );
    add(
        &mut t,
        "uu_command_exec",
        "通过 UU 终端执行 Windows PowerShell 命令。返回 task_id；结果含合并输出和确认后的退出码。取消会关闭命令专属终端。",
        &["session_id", "command"],
        session(&[
            ("command", string()),
            ("cwd", string()),
            ("timeout_ms", integer()),
        ]),
        false,
    );
    add(
        &mut t,
        "uu_files_list",
        "浏览远端目录。默认 C:\\，其他盘请指定 path；分页用 offset/limit。",
        &["session_id"],
        session(&[
            ("path", string()),
            ("offset", integer()),
            ("limit", integer()),
        ]),
        true,
    );
    for (name, desc, props, required) in [
        (
            "uu_files_mkdir",
            "创建远端目录",
            session(&[("path", string())]),
            vec!["session_id", "path"],
        ),
        (
            "uu_files_rename",
            "重命名远端项目，name 仅为新文件名",
            session(&[("path", string()), ("name", string())]),
            vec!["session_id", "path", "name"],
        ),
        (
            "uu_files_delete",
            "删除指定远端文件或目录。目录需 directory=true，递归删除需 recursive=true",
            session(&[
                ("path", string()),
                ("directory", boolean()),
                ("recursive", boolean()),
            ]),
            vec!["session_id", "path"],
        ),
    ] {
        add(&mut t, name, desc, &required, props, false);
    }
    let item = json!({"type":"object","properties":fields(&[("local_path",string()),("remote_path",string()),("directory",boolean())]),"required":["local_path","remote_path"],"additionalProperties":false});
    for (name, desc) in [
        (
            "uu_files_upload",
            "上传 Linux 本地文件/目录至远端目录，支持 items 批量，返回 task_id。同名上传跳过，不覆盖。",
        ),
        (
            "uu_files_download",
            "下载远端文件/目录至 Linux 本地目录，支持 items 批量，返回 task_id。目录需 directory=true。同名下载失败，不覆盖。",
        ),
    ] {
        add(
            &mut t,
            name,
            desc,
            &["session_id"],
            session(&[
                ("local_path", string()),
                ("remote_path", string()),
                ("directory", boolean()),
                (
                    "items",
                    json!({"type":"array","items":item,"minItems":1,"maxItems":1000}),
                ),
            ]),
            false,
        );
    }
    add(
        &mut t,
        "uu_transfer_control",
        "暂停、继续或取消本客户端的批量传输任务",
        &["task_id", "action"],
        fields(&[
            ("task_id", string()),
            ("action", enumeration(&["pause", "resume", "cancel"])),
        ]),
        false,
    );
    add(
        &mut t,
        "uu_port_create",
        "创建 TCP 端口映射，监听 Linux 本机 127.0.0.1；target_host 从被控端解析。成功监听不等于已连通目标。默认保存配置，退出后关闭监听；下次需显式启动。persist=false 创建临时规则。",
        &["session_id", "local_port", "target_port"],
        session(&[
            ("name", string()),
            ("local_port", integer()),
            ("target_host", string()),
            ("target_port", integer()),
            ("enabled", boolean()),
            ("persist", boolean()),
        ]),
        false,
    );
    add(
        &mut t,
        "uu_port_list",
        "列出会话端口映射及归属",
        &["session_id"],
        session(&[]),
        true,
    );
    for name in ["uu_port_start", "uu_port_stop", "uu_port_delete"] {
        add(
            &mut t,
            name,
            "启用、停用或删除本客户端的端口映射",
            &["session_id", "rule_id"],
            session(&[("rule_id", integer())]),
            false,
        );
    }
    add(
        &mut t,
        "uu_port_update",
        "修改本客户端的端口映射；启用中的规则会重启监听",
        &["session_id", "rule_id"],
        session(&[
            ("rule_id", integer()),
            ("name", string()),
            ("local_port", integer()),
            ("target_host", string()),
            ("target_port", integer()),
        ]),
        false,
    );
    for name in ["uu_task_get", "uu_task_cancel"] {
        add(
            &mut t,
            name,
            "查询或取消本客户端的命令、文件传输、屏幕设置任务",
            &["task_id"],
            fields(&[("task_id", string())]),
            name == "uu_task_get",
        );
    }
    add(
        &mut t,
        "uu_events_poll",
        "读取会话事件。cursor 从上次响应续读；wait_ms 最多 10000。gap=true 表示旧事件已过期。",
        &["session_id"],
        session(&[("cursor", integer()), ("wait_ms", integer())]),
        true,
    );
    for (name, desc) in [
        (
            "uu_mcp_status",
            "查看共享服务、客户端和任务状态，以及 MCP 配置",
        ),
        ("uu_favorites_list", "列出收藏设备，不返回保存的验证码"),
    ] {
        add(&mut t, name, desc, &[], json!({}), true);
    }
    add(
        &mut t,
        "uu_favorites_save",
        "添加或修改收藏；未提供的名称和验证码保持原值",
        &["id"],
        fields(&[("id", string()), ("name", string()), ("code", string())]),
        false,
    );
    add(
        &mut t,
        "uu_favorites_delete",
        "删除本地收藏",
        &["id"],
        fields(&[("id", string())]),
        false,
    );
    add(
        &mut t,
        "uu_device_rename",
        "修改账号设备名称，最多 40 字",
        &["device_id", "name"],
        fields(&[("device_id", string()), ("name", string())]),
        false,
    );
    add(
        &mut t,
        "uu_terminal_open",
        "打开交互终端；同一设备同一时刻可用一个终端，GUI 与 MCP 共用此限制",
        &["session_id"],
        session(&[
            ("shell", enumeration(&["powershell"])),
            ("cols", integer()),
            ("rows", integer()),
        ]),
        false,
    );
    for (name, desc, extra, required, read) in [
        (
            "uu_terminal_read",
            "读取终端增量输出；cursor 是字节位置，返回 text 和无损 data_base64",
            vec![("cursor", integer()), ("wait_ms", integer())],
            vec![],
            true,
        ),
        (
            "uu_terminal_write",
            "向终端写入文字；执行一行时在末尾添加回车 \r",
            vec![("text", string())],
            vec!["text"],
            false,
        ),
        (
            "uu_terminal_resize",
            "修改终端字符网格尺寸",
            vec![("cols", integer()), ("rows", integer())],
            vec!["cols", "rows"],
            false,
        ),
        (
            "uu_terminal_close",
            "关闭远端终端进程并确认退出，用于中断正在执行的命令",
            vec![],
            vec![],
            false,
        ),
    ] {
        let mut props = session(&extra);
        props["terminal_id"] = string();
        let mut req = vec!["session_id", "terminal_id"];
        req.extend(required);
        add(&mut t, name, desc, &req, props, read);
    }
    add(
        &mut t,
        "uu_clipboard_get",
        "读取远端文本剪贴板；不修改 Linux 本地剪贴板",
        &["session_id"],
        session(&[]),
        true,
    );
    add(
        &mut t,
        "uu_clipboard_set",
        "写入远端文本剪贴板；不修改 Linux 本地剪贴板",
        &["session_id", "text"],
        session(&[("text", string())]),
        false,
    );
    add(
        &mut t,
        "uu_files_read_text",
        "下载并读取不超过 1 MiB 的 UTF-8 或带 BOM 的 UTF-16 文本；返回 task_id",
        &["session_id", "path"],
        session(&[("path", string())]),
        true,
    );
    add(
        &mut t,
        "uu_files_write_text",
        "新建 UTF-8 文本文件，最多 1 MiB；同名文件不覆盖，返回 task_id",
        &["session_id", "path", "text"],
        session(&[("path", string()), ("text", string())]),
        false,
    );
    add(
        &mut t,
        "uu_files_search",
        "在指定目录递归按名称包含匹配搜索，返回 task_id；最多 2000 个目录、32 层",
        &["session_id", "path", "query"],
        session(&[
            ("path", string()),
            ("query", string()),
            ("limit", integer()),
        ]),
        true,
    );
    t
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn catalog_names_are_unique_and_schemas_reject_unknown_fields() {
        let tools = catalog();
        let mut names = std::collections::HashSet::new();
        for tool in tools {
            assert!(names.insert(tool.name.clone()));
            assert_eq!(tool.input_schema["additionalProperties"], false);
        }
    }
}
